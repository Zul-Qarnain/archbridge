//! A bounded, panic-free reader for the shared libraries an ELF object needs.
//!
//! Foreign `.deb`/`.rpm` payloads are untrusted data, so every offset, size and
//! count in this file comes from the object being read and is validated against
//! the object's own length before it is used. Anything that does not add up —
//! a truncated table, an out-of-range section, an absurd entry count — answers
//! with "no libraries", never with a panic and never with an unbounded
//! allocation.

use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

const ELF_MAGIC: [u8; 4] = [0x7f, b'E', b'L', b'F'];
const ELFCLASS32: u8 = 1;
const ELFCLASS64: u8 = 2;
const ELFDATA2LSB: u8 = 1;
const ELFDATA2MSB: u8 = 2;

const PT_LOAD: u32 = 1;
const PT_DYNAMIC: u32 = 2;
const SHT_STRTAB: u32 = 3;
const SHT_DYNAMIC: u32 = 6;

const DT_NULL: i64 = 0;
const DT_NEEDED: i64 = 1;
const DT_STRTAB: i64 = 5;

/// How many candidate payload files a single inspection examines.
pub const MAX_ELF_FILES: usize = 64;

/// No table or string block is ever read further than this.
const MAX_TABLE_BYTES: u64 = 1 << 20;
const MAX_SECTION_HEADERS: u64 = 4096;
const MAX_PROGRAM_HEADERS: u64 = 256;
const MAX_DYNAMIC_ENTRIES: usize = 2048;
const MAX_NEEDED_PER_FILE: usize = 128;
const MAX_LIBRARY_NAME_BYTES: usize = 1024;

/// Both classes fit their whole identity record inside this many bytes.
const HEADER_BYTES: usize = 64;

struct Section {
    stype: u32,
    addr: u64,
    offset: u64,
    size: u64,
}

struct Segment {
    ptype: u32,
    offset: u64,
    vaddr: u64,
    filesz: u64,
    memsz: u64,
}

/// The library names an ELF object declares as `DT_NEEDED`, in the order a
/// loader would see them. Empty for anything that is not a parseable ELF
/// object, which is what lets this point at arbitrary payload files.
pub fn needed_libraries_in_file(path: &Path) -> Vec<String> {
    let meta = match path.metadata() {
        Ok(meta) => meta,
        Err(_) => return Vec::new(),
    };
    // Size is not a reason to skip an object: only the identity record, the two
    // header tables and the dynamic and string blocks are ever read, each under
    // `MAX_TABLE_BYTES`. A 400 MB binary with debug info costs the same as a 40 KB
    // one, and its library list is exactly as reviewable.
    if !meta.is_file() || meta.len() < HEADER_BYTES as u64 {
        return Vec::new();
    }
    let mut file = match fs::File::open(path) {
        Ok(file) => file,
        Err(_) => return Vec::new(),
    };
    needed_libraries(&mut file, meta.len())
}

pub fn needed_libraries<R: Read + Seek>(source: &mut R, size: u64) -> Vec<String> {
    let header = match read_at(source, 0, HEADER_BYTES as u64, size) {
        Some(header) => header,
        None => return Vec::new(),
    };
    let object = match Object::parse(&header, size) {
        Some(object) => object,
        None => return Vec::new(),
    };

    let sections = object.load_sections(source, size);
    let segments = object.load_segments(source, size);

    let dynamic = match object.find_dynamic(&sections, &segments) {
        Some(dynamic) => dynamic,
        None => return Vec::new(),
    };
    let entries = object.read_dynamic(source, size, dynamic);
    if entries.is_empty() {
        return Vec::new();
    }

    // DT_STRTAB holds a virtual address, not a file offset, so it has to be
    // mapped back through the section headers or — for stripped objects that
    // kept no section headers — through the loaded segments.
    let strtab_address = match entries
        .iter()
        .find(|(tag, _)| *tag == DT_STRTAB)
        .map(|(_, value)| *value)
    {
        Some(address) => address,
        None => return Vec::new(),
    };
    let (offset, length) = match resolve_address(strtab_address, &sections, &segments, size) {
        Some(resolved) => resolved,
        None => return Vec::new(),
    };
    let strtab = match read_at(source, offset, length, size) {
        Some(strtab) => strtab,
        None => return Vec::new(),
    };

    let mut names: Vec<String> = Vec::new();
    for (tag, value) in entries {
        if tag != DT_NEEDED {
            continue;
        }
        if let Some(name) = cstr_at(&strtab, value) {
            if !names.contains(&name) {
                names.push(name);
            }
            if names.len() >= MAX_NEEDED_PER_FILE {
                break;
            }
        }
    }
    names
}

/// Field widths and table positions taken from the identity record.
struct Object {
    class64: bool,
    le: bool,
    section_offset: u64,
    section_count: u64,
    section_entry_size: u64,
    program_offset: u64,
    program_count: u64,
    program_entry_size: u64,
}

impl Object {
    fn parse(header: &[u8], size: u64) -> Option<Self> {
        if header.len() < HEADER_BYTES || header[0..4] != ELF_MAGIC {
            return None;
        }
        let class64 = match header[4] {
            ELFCLASS32 => false,
            ELFCLASS64 => true,
            _ => return None,
        };
        let le = match header[5] {
            ELFDATA2LSB => true,
            ELFDATA2MSB => false,
            _ => return None,
        };
        // The 32-bit identity record is 52 bytes, the 64-bit one 64.
        if size < (if class64 { 64 } else { 52 }) {
            return None;
        }
        let (phoff, shoff, phentsize, phnum, shentsize, shnum) = if class64 {
            (
                read_u64(header, 32, le)?,
                read_u64(header, 40, le)?,
                u64::from(read_u16(header, 54, le)?),
                read_u16(header, 56, le)?,
                u64::from(read_u16(header, 58, le)?),
                read_u16(header, 60, le)?,
            )
        } else {
            (
                u64::from(read_u32(header, 28, le)?),
                u64::from(read_u32(header, 32, le)?),
                u64::from(read_u16(header, 42, le)?),
                read_u16(header, 44, le)?,
                u64::from(read_u16(header, 46, le)?),
                read_u16(header, 48, le)?,
            )
        };
        Some(Object {
            class64,
            le,
            section_offset: shoff,
            section_count: bounded_count(shnum, MAX_SECTION_HEADERS),
            section_entry_size: shentsize,
            program_offset: phoff,
            program_count: bounded_count(phnum, MAX_PROGRAM_HEADERS),
            program_entry_size: phentsize,
        })
    }

    fn load_sections<R: Read + Seek>(&self, source: &mut R, size: u64) -> Vec<Section> {
        let minimum = if self.class64 { 64 } else { 40 };
        let stride_u64 = self.section_entry_size;
        let Some(table) = self.table(self.section_offset, self.section_count, stride_u64, minimum)
        else {
            return Vec::new();
        };
        let Some(bytes) = read_at(source, self.section_offset, table, size) else {
            return Vec::new();
        };
        let stride = stride_u64 as usize;
        let mut sections = Vec::new();
        let mut at = 0;
        while at + stride <= bytes.len() && sections.len() < self.section_count as usize {
            if let Some(section) = self.parse_section(&bytes[at..at + stride]) {
                sections.push(section);
            }
            at += stride;
        }
        sections
    }

    fn parse_section(&self, entry: &[u8]) -> Option<Section> {
        if self.class64 {
            Some(Section {
                stype: read_u32(entry, 4, self.le)?,
                addr: read_u64(entry, 16, self.le)?,
                offset: read_u64(entry, 24, self.le)?,
                size: read_u64(entry, 32, self.le)?,
            })
        } else {
            Some(Section {
                stype: read_u32(entry, 4, self.le)?,
                addr: u64::from(read_u32(entry, 12, self.le)?),
                offset: u64::from(read_u32(entry, 16, self.le)?),
                size: u64::from(read_u32(entry, 20, self.le)?),
            })
        }
    }

    fn load_segments<R: Read + Seek>(&self, source: &mut R, size: u64) -> Vec<Segment> {
        let minimum = if self.class64 { 56 } else { 32 };
        let stride_u64 = self.program_entry_size;
        let Some(table) = self.table(self.program_offset, self.program_count, stride_u64, minimum)
        else {
            return Vec::new();
        };
        let Some(bytes) = read_at(source, self.program_offset, table, size) else {
            return Vec::new();
        };
        let stride = stride_u64 as usize;
        let mut segments = Vec::new();
        let mut at = 0;
        while at + stride <= bytes.len() && segments.len() < self.program_count as usize {
            if let Some(segment) = self.parse_segment(&bytes[at..at + stride]) {
                segments.push(segment);
            }
            at += stride;
        }
        segments
    }

    fn parse_segment(&self, entry: &[u8]) -> Option<Segment> {
        if self.class64 {
            Some(Segment {
                ptype: read_u32(entry, 0, self.le)?,
                offset: read_u64(entry, 8, self.le)?,
                vaddr: read_u64(entry, 16, self.le)?,
                filesz: read_u64(entry, 32, self.le)?,
                memsz: read_u64(entry, 40, self.le)?,
            })
        } else {
            Some(Segment {
                ptype: read_u32(entry, 0, self.le)?,
                offset: u64::from(read_u32(entry, 4, self.le)?),
                vaddr: u64::from(read_u32(entry, 8, self.le)?),
                filesz: u64::from(read_u32(entry, 16, self.le)?),
                memsz: u64::from(read_u32(entry, 20, self.le)?),
            })
        }
    }

    /// The size of a header table, refused when its stride or span is not real.
    fn table(&self, offset: u64, count: u64, stride: u64, minimum_stride: u64) -> Option<u64> {
        if offset == 0 || count == 0 || stride < minimum_stride {
            return None;
        }
        let total = count.checked_mul(stride)?;
        offset.checked_add(total)?;
        Some(total.min(MAX_TABLE_BYTES))
    }

    /// The `.dynamic` block: a section when the object kept section headers,
    /// the PT_DYNAMIC segment when it did not.
    fn find_dynamic(&self, sections: &[Section], segments: &[Segment]) -> Option<(u64, u64)> {
        if let Some(section) = sections
            .iter()
            .find(|section| section.stype == SHT_DYNAMIC && section.size > 0)
        {
            return Some((section.offset, section.size));
        }
        segments
            .iter()
            .find(|segment| segment.ptype == PT_DYNAMIC && segment.filesz > 0)
            .map(|segment| (segment.offset, segment.filesz))
    }

    fn read_dynamic<R: Read + Seek>(
        &self,
        source: &mut R,
        size: u64,
        dynamic: (u64, u64),
    ) -> Vec<(i64, u64)> {
        let (offset, length) = dynamic;
        let bytes = match read_at(source, offset, length, size) {
            Some(bytes) => bytes,
            None => return Vec::new(),
        };
        let word = if self.class64 { 8 } else { 4 };
        let stride = word * 2;
        let mut entries: Vec<(i64, u64)> = Vec::new();
        let mut at = 0;
        while at + stride <= bytes.len() && entries.len() < MAX_DYNAMIC_ENTRIES {
            let tag = if self.class64 {
                read_u64(&bytes, at, self.le).map(|raw| raw as i64)
            } else {
                read_u32(&bytes, at, self.le).map(|raw| raw as i32 as i64)
            };
            let value = if self.class64 {
                read_u64(&bytes, at + word, self.le)
            } else {
                read_u32(&bytes, at + word, self.le).map(u64::from)
            };
            let (tag, value) = match (tag, value) {
                (Some(tag), Some(value)) => (tag, value),
                _ => break,
            };
            if tag == DT_NULL {
                break;
            }
            entries.push((tag, value));
            at += stride;
        }
        entries
    }
}

/// `0xFFFF` is the escape value that points at an extended count this reader
/// does not follow, so it reads as "no table". Counts are additionally clamped
/// to a ceiling rather than trusted.
fn bounded_count(raw: u16, ceiling: u64) -> u64 {
    match raw {
        0 | u16::MAX => 0,
        count => u64::from(count).min(ceiling),
    }
}

/// Map a virtual address onto `(file offset, readable length)` within the file.
fn resolve_address(
    address: u64,
    sections: &[Section],
    segments: &[Segment],
    size: u64,
) -> Option<(u64, u64)> {
    for section in sections
        .iter()
        .filter(|section| section.stype == SHT_STRTAB)
    {
        if let Some(delta) = address.checked_sub(section.addr) {
            if delta < section.size {
                return within_file(
                    section.offset.checked_add(delta)?,
                    section.size - delta,
                    size,
                );
            }
        }
    }
    for segment in segments.iter().filter(|segment| segment.ptype == PT_LOAD) {
        if let Some(delta) = address.checked_sub(segment.vaddr) {
            if delta < segment.memsz && delta < segment.filesz {
                return within_file(
                    segment.offset.checked_add(delta)?,
                    segment.filesz - delta,
                    size,
                );
            }
        }
    }
    None
}

fn within_file(offset: u64, length: u64, size: u64) -> Option<(u64, u64)> {
    if length == 0 || offset >= size {
        return None;
    }
    let end = offset.checked_add(length)?;
    if end > size {
        return None;
    }
    Some((offset, length))
}

/// A bounded, NUL-terminated read out of the string table.
fn cstr_at(bytes: &[u8], at: u64) -> Option<String> {
    let start = usize::try_from(at).ok()?;
    let end = bytes.len().min(start.checked_add(MAX_LIBRARY_NAME_BYTES)?);
    let window = bytes.get(start..end)?;
    let terminator = window.iter().position(|byte| *byte == 0)?;
    let name = match std::str::from_utf8(&window[..terminator]) {
        Ok(name) => name,
        Err(_) => return None,
    };
    // A soname is never empty, never a path, and a `@`-prefixed entry is an
    // rpath placeholder rather than a library the loader will look up by name.
    if name.is_empty() || name.contains('/') || name.starts_with('@') {
        return None;
    }
    Some(name.to_string())
}

fn read_u16(bytes: &[u8], at: usize, le: bool) -> Option<u16> {
    let slice: &[u8; 2] = bytes.get(at..at.checked_add(2)?)?.try_into().ok()?;
    Some(if le {
        u16::from_le_bytes(*slice)
    } else {
        u16::from_be_bytes(*slice)
    })
}

fn read_u32(bytes: &[u8], at: usize, le: bool) -> Option<u32> {
    let slice: &[u8; 4] = bytes.get(at..at.checked_add(4)?)?.try_into().ok()?;
    Some(if le {
        u32::from_le_bytes(*slice)
    } else {
        u32::from_be_bytes(*slice)
    })
}

fn read_u64(bytes: &[u8], at: usize, le: bool) -> Option<u64> {
    let slice: &[u8; 8] = bytes.get(at..at.checked_add(8)?)?.try_into().ok()?;
    Some(if le {
        u64::from_le_bytes(*slice)
    } else {
        u64::from_be_bytes(*slice)
    })
}

/// Reads `length` bytes at `offset`, refusing anything that runs past the end
/// of the object or beyond the per-table cap.
fn read_at<R: Read + Seek>(source: &mut R, offset: u64, length: u64, size: u64) -> Option<Vec<u8>> {
    if length == 0 || length > MAX_TABLE_BYTES {
        return None;
    }
    let end = offset.checked_add(length)?;
    if end > size {
        return None;
    }
    if source.seek(SeekFrom::Start(offset)).is_err() {
        return None;
    }
    let mut buffer = vec![0u8; length as usize];
    if source.read_exact(&mut buffer).is_err() {
        return None;
    }
    Some(buffer)
}

/// Assembles an ELF image that declares exactly the given `DT_NEEDED` entries,
/// laid out the way a linker lays out a real object: a `.dynamic` block whose
/// `DT_STRTAB` names a string table, addressed the way a loader sees it.
///
/// Product code only ever *reads* such images. This exists so a test can check a
/// reader against an object whose contents are known exactly, on a host with no
/// toolchain and no reference binary to borrow.
pub fn build_fixture_object(
    class64: bool,
    le: bool,
    with_sections: bool,
    with_segments: bool,
    needed: &[&str],
) -> Vec<u8> {
    let header_size = if class64 { 64 } else { 52 };
    let phentsize = if class64 { 56 } else { 32 };
    let shentsize = if class64 { 64 } else { 40 };
    let word = if class64 { 8 } else { 4 };
    // DT_STRTAB, one DT_NEEDED per library, and the terminating DT_NULL.
    let dynamic_size = (2 + needed.len()) * (word * 2);

    let dynstr = string_table(needed);
    let dynstr_names = needed_indices(needed);
    let shstrtab = b"\0.dynamic\0.dynstr\0";

    let mut cursor = header_size;
    let phoff = cursor;
    if with_segments {
        cursor += 2 * phentsize;
    }
    let dynamic_offset = cursor;
    cursor += dynamic_size;
    let dynstr_offset = cursor;
    cursor += dynstr.len();
    let shstrtab_offset = cursor;
    if with_sections {
        cursor += shstrtab.len();
    }
    let shoff = cursor;
    let total = cursor + if with_sections { 4 * shentsize } else { 0 };

    let mut sink = Sink {
        bytes: Vec::new(),
        le,
        class64,
    };

    // ---- identity record
    sink.raw(&ELF_MAGIC);
    sink.bytes
        .push(if class64 { ELFCLASS64 } else { ELFCLASS32 });
    sink.bytes.push(if le { ELFDATA2LSB } else { ELFDATA2MSB });
    sink.bytes.push(1); // e_ident[EI_VERSION]
    while sink.bytes.len() < 16 {
        sink.bytes.push(0);
    }
    sink.u16(3); // e_type: ET_DYN
    sink.u16(62); // e_machine
    sink.u32(1); // e_version
    sink.word(0); // e_entry
    sink.word(if with_segments { phoff } else { 0 } as u64);
    sink.word(if with_sections { shoff } else { 0 } as u64);
    sink.u32(0); // e_flags
    sink.u16(header_size as u16); // e_ehsize
    sink.u16(if with_segments { phentsize } else { 0 } as u16);
    sink.u16(if with_segments { 2 } else { 0 }); // e_phnum
    sink.u16(if with_sections { shentsize } else { 0 } as u16);
    sink.u16(if with_sections { 4 } else { 0 }); // e_shnum
    sink.u16(if with_sections { 3 } else { 0 }); // e_shstrndx

    // ---- program headers: one load segment covering the file, one dynamic
    if with_segments {
        sink.u32(PT_LOAD);
        if class64 {
            sink.u32(5); // p_flags comes before the addresses in ELF64
        }
        sink.word(0); // p_offset
        sink.word(0); // p_vaddr: identity mapping, as a single-segment object uses
        sink.word(0); // p_paddr
        sink.word(total as u64); // p_filesz
        sink.word(total as u64); // p_memsz
        if !class64 {
            sink.u32(5);
        }
        sink.word(0x1000); // p_align

        sink.u32(PT_DYNAMIC);
        if class64 {
            sink.u32(6);
        }
        sink.word(dynamic_offset as u64); // p_offset
        sink.word(dynamic_offset as u64); // p_vaddr
        sink.word(0); // p_paddr
        sink.word(dynamic_size as u64); // p_filesz
        sink.word(dynamic_size as u64); // p_memsz
        if !class64 {
            sink.u32(6);
        }
        sink.word(8); // p_align
    }

    // ---- .dynamic
    sink.word(DT_STRTAB as u64);
    sink.word(dynstr_offset as u64);
    for index in &dynstr_names {
        sink.word(DT_NEEDED as u64);
        sink.word(*index);
    }
    sink.word(DT_NULL as u64);
    sink.word(0);

    // ---- string tables
    sink.raw(&dynstr);
    if with_sections {
        sink.raw(shstrtab);

        let section =
            |sink: &mut Sink, name: u32, stype: u32, addr: u64, offset: u64, size: u64| {
                sink.u32(name);
                sink.u32(stype);
                if class64 {
                    sink.word(0); // sh_flags
                    sink.word(addr);
                    sink.word(offset);
                    sink.word(size);
                    sink.u32(0); // sh_link
                    sink.u32(0); // sh_info
                    sink.word(1); // sh_addralign
                    sink.word(0); // sh_entsize
                } else {
                    sink.u32(0);
                    sink.u32(addr as u32);
                    sink.u32(offset as u32);
                    sink.u32(size as u32);
                    sink.u32(0);
                    sink.u32(0);
                    sink.u32(1);
                    sink.u32(0);
                }
            };
        // null, .dynamic, .dynstr, .shstrtab
        section(&mut sink, 0, 0, 0, 0, 0);
        section(
            &mut sink,
            1,
            SHT_DYNAMIC,
            dynamic_offset as u64,
            dynamic_offset as u64,
            dynamic_size as u64,
        );
        section(
            &mut sink,
            10,
            SHT_STRTAB,
            dynstr_offset as u64,
            dynstr_offset as u64,
            dynstr.len() as u64,
        );
        section(
            &mut sink,
            18,
            SHT_STRTAB,
            0,
            shstrtab_offset as u64,
            shstrtab.len() as u64,
        );
    }

    assert_eq!(sink.bytes.len(), total, "fixture layout drifted");
    sink.bytes
}

/// The `.dynstr` block: a leading NUL, then each name NUL-terminated.
fn string_table(needed: &[&str]) -> Vec<u8> {
    let mut table = vec![0u8];
    for name in needed {
        table.extend_from_slice(name.as_bytes());
        table.push(0);
    }
    table
}

/// Where each name sits in the table `string_table` builds.
fn needed_indices(needed: &[&str]) -> Vec<u64> {
    let mut offsets = Vec::new();
    let mut at = 1usize;
    for name in needed {
        offsets.push(at as u64);
        at += name.len() + 1;
    }
    offsets
}

/// Writes the fixture's fields in the byte order the object claims.
struct Sink {
    bytes: Vec<u8>,
    le: bool,
    class64: bool,
}

impl Sink {
    fn raw(&mut self, value: &[u8]) {
        self.bytes.extend_from_slice(value);
    }
    fn u16(&mut self, value: u16) {
        self.raw(
            &(if self.le {
                value.to_le_bytes()
            } else {
                value.to_be_bytes()
            }),
        );
    }
    fn u32(&mut self, value: u32) {
        self.raw(
            &(if self.le {
                value.to_le_bytes()
            } else {
                value.to_be_bytes()
            }),
        );
    }
    fn word(&mut self, value: u64) {
        if self.class64 {
            self.raw(
                &(if self.le {
                    value.to_le_bytes()
                } else {
                    value.to_be_bytes()
                }),
            );
        } else {
            self.u32(value as u32);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    const LIBRARIES: [&str; 2] = ["libc.so.6", "libm.so.6"];

    fn object(class64: bool, le: bool, with_sections: bool, with_segments: bool) -> Vec<u8> {
        build_fixture_object(class64, le, with_sections, with_segments, &LIBRARIES)
    }

    fn libraries(bytes: &[u8]) -> Vec<String> {
        let mut cursor = Cursor::new(bytes.to_vec());
        needed_libraries(&mut cursor, bytes.len() as u64)
    }

    const EXPECTED: [&str; 2] = ["libc.so.6", "libm.so.6"];

    #[test]
    fn elf64_little_endian_object_reports_its_needed_libraries() {
        assert_eq!(
            libraries(&object(true, true, true, false)),
            EXPECTED.to_vec()
        );
    }

    #[test]
    fn big_endian_and_32_bit_objects_are_read_in_their_own_byte_order() {
        for (class64, le) in [(true, false), (false, true), (false, false)] {
            assert_eq!(
                libraries(&object(class64, le, true, false)),
                EXPECTED.to_vec(),
                "class64={class64} le={le}"
            );
        }
    }

    #[test]
    fn stripped_object_falls_back_to_the_program_headers() {
        for class64 in [true, false] {
            assert_eq!(
                libraries(&object(class64, true, false, true)),
                EXPECTED.to_vec(),
                "class64={class64}"
            );
        }
    }

    #[test]
    fn a_third_library_is_appended_in_the_order_the_loader_sees_it() {
        let bytes = build_fixture_object(
            true,
            true,
            true,
            false,
            &["libarchive.so.13", "libc.so.6", "ld-linux-x86-64.so.2"],
        );
        assert_eq!(
            libraries(&bytes),
            vec![
                "libarchive.so.13".to_string(),
                "libc.so.6".to_string(),
                "ld-linux-x86-64.so.2".to_string()
            ]
        );
    }

    #[test]
    fn an_object_without_any_dynamic_table_names_nothing() {
        assert!(libraries(&object(true, true, false, false)).is_empty());
    }

    #[test]
    fn every_truncation_of_an_object_is_refused_without_a_panic() {
        let bytes = object(true, true, true, true);
        for cut in 0..bytes.len() {
            let mut cursor = Cursor::new(bytes[..cut].to_vec());
            let _ = needed_libraries(&mut cursor, cut as u64);
        }
        // Only the identity record survives these cuts: every table lies beyond.
        assert!(libraries(&bytes[..64]).is_empty());
        assert!(libraries(&bytes[..100]).is_empty());
    }

    #[test]
    fn non_elf_bytes_are_not_libraries() {
        assert!(libraries(b"\x7fELF not really an object at all").is_empty());
        assert!(libraries(&[0u8; 256]).is_empty());
    }

    #[test]
    fn absurd_table_counts_and_offsets_are_ignored() {
        for class64 in [true, false] {
            // e_shnum / e_phnum = 0xFFFF means "the real count is elsewhere".
            let (shnum_at, phnum_at) = if class64 { (60, 56) } else { (48, 44) };
            let mut bytes = object(class64, true, true, true);
            bytes[shnum_at] = 0xff;
            bytes[shnum_at + 1] = 0xff;
            bytes[phnum_at] = 0xff;
            bytes[phnum_at + 1] = 0xff;
            assert!(libraries(&bytes).is_empty(), "escape counts were followed");

            // An out-of-file section table must be refused, and with no program
            // headers there is nothing left to fall back on.
            let (shoff_at, phnum_at) = if class64 { (40, 56) } else { (32, 44) };
            let mut bytes = object(class64, true, true, true);
            bytes[phnum_at] = 0;
            bytes[phnum_at + 1] = 0;
            let huge: Vec<u8> = if class64 {
                (u64::MAX / 2).to_le_bytes().to_vec()
            } else {
                (u32::MAX - 1).to_le_bytes().to_vec()
            };
            for (index, byte) in huge.iter().enumerate() {
                bytes[shoff_at + index] = *byte;
            }
            assert!(libraries(&bytes).is_empty(), "out-of-file table was read");
        }
    }

    #[test]
    fn string_table_indices_outside_the_table_are_dropped() {
        let mut bytes = object(true, true, true, false);
        // The first DT_NEEDED value follows the identity record and the
        // DT_STRTAB pair; pointing it past the table must drop that entry only.
        let value_at = 64 + 2 * 8;
        for index in 0..8 {
            bytes[value_at + index] = 0xff;
        }
        assert_eq!(libraries(&bytes), vec!["libm.so.6".to_string()]);
    }

    #[test]
    fn a_directory_and_a_text_file_are_not_objects() {
        let dir = tempfile::tempdir().unwrap();
        assert!(needed_libraries_in_file(dir.path()).is_empty());
        let text = dir.path().join("README");
        fs::write(&text, b"plain text").unwrap();
        assert!(needed_libraries_in_file(&text).is_empty());
    }

    #[test]
    fn a_real_object_on_disk_round_trips_through_the_file_entry_point() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("libfake.so.1");
        fs::write(&path, object(true, true, true, true)).unwrap();
        assert_eq!(needed_libraries_in_file(&path), EXPECTED.to_vec());
    }
}
