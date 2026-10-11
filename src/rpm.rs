//! Decodes the metadata half of an RPM file: its lead, its signature header and
//! the main header that carries names, dependencies and scriptlets.
//!
//! The payload is deliberately out of reach here. RPM stores `%pre`/`%post`
//! scriptlets as plain strings in the header, so the reviewable half of a
//! package can be read without touching — let alone running — anything the
//! maintainer wrote. Every length and offset in a header comes from the file,
//! so each one is validated against the bytes that are actually present before
//! it is used: a truncated or hostile package is refused with an error, never
//! with a panic or an allocation sized from its own counters.

use serde::{Deserialize, Serialize};

/// The fixed 96-byte lead that opens every RPM file.
const LEAD_SIZE: usize = 96;
const LEAD_MAGIC: [u8; 2] = [0xed, 0xab];
const RPM_MAGIC: [u8; 3] = [0x8e, 0xad, 0xe8];
const HEADER_SIZE: usize = 16;
const INDEX_ENTRY_SIZE: usize = 16;

/// A header region is `16 + n*16 + dataLen` bytes; both counters come from the
/// file, so both are capped before they can size an allocation.
const MAX_INDEX_ENTRIES: u32 = 200_000;
const MAX_ARRAY_ITEMS: u32 = 100_000;
const MAX_STRING_BYTES: usize = 1 << 20;

/// Tag data types, as stored in the index.
pub const TYPE_INT8: u32 = 2;
pub const TYPE_INT16: u32 = 3;
pub const TYPE_INT32: u32 = 4;
pub const TYPE_INT64: u32 = 5;
pub const TYPE_STRING: u32 = 6;
pub const TYPE_BIN: u32 = 7;
pub const TYPE_STRING_ARRAY: u32 = 8;
pub const TYPE_I18N_STRING: u32 = 9;
/// `CHAR` is an 8-bit type that predates `INT8` and is still written by rpm.
pub const TYPE_CHAR: u32 = 1;

/// Header tags this reader follows.
pub const TAG_NAME: u32 = 1000;
pub const TAG_VERSION: u32 = 1001;
pub const TAG_RELEASE: u32 = 1002;
pub const TAG_EPOCH: u32 = 1003;
pub const TAG_SUMMARY: u32 = 1004;
pub const TAG_DESCRIPTION: u32 = 1005;
pub const TAG_ARCH: u32 = 1022;
pub const TAG_PREIN: u32 = 1023;
pub const TAG_POSTIN: u32 = 1024;
pub const TAG_PREUN: u32 = 1025;
pub const TAG_POSTUN: u32 = 1026;
pub const TAG_URL: u32 = 1020;
pub const TAG_PACKAGER: u32 = 1015;
pub const TAG_LICENSE: u32 = 1014;
pub const TAG_PROVIDENAME: u32 = 1047;
pub const TAG_REQUIRENAME: u32 = 1049;
pub const TAG_REQUIREFLAGS: u32 = 1048;
pub const TAG_REQUIREVERSION: u32 = 1050;
pub const TAG_PREINPROG: u32 = 1085;
pub const TAG_POSTINPROG: u32 = 1086;
pub const TAG_PREUNPROG: u32 = 1087;
pub const TAG_POSTUNPROG: u32 = 1088;
pub const TAG_BASENAMES: u32 = 1117;
pub const TAG_PAYLOADFORMAT: u32 = 1124;
pub const TAG_PAYLOADCOMPRESSOR: u32 = 1125;
pub const TAG_PRETRANS: u32 = 1151;
pub const TAG_POSTTRANS: u32 = 1152;
pub const TAG_PRETRANSPROG: u32 = 1153;
pub const TAG_POSTTRANSPROG: u32 = 1154;

/// Dependency sense bits (rpm's `RPMSENSE_*`).
pub const SENSE_LESS: i32 = 1 << 1;
pub const SENSE_GREATER: i32 = 1 << 2;
pub const SENSE_EQUAL: i32 = 1 << 3;
pub const SENSE_MASK: i32 = 0x0f;
pub const SENSE_CONFIG: i32 = 1 << 28;
pub const SENSE_RPMLIB: i32 = 1 << 24;

/// A scriptlet the installer would run. Kept as reviewable text only.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Scriptlet {
    /// rpm's own name for the phase: `prein`, `posttrans`, …
    pub name: String,
    /// The interpreter rpm would exec, when the spec named one.
    pub interpreter: Option<String>,
    pub body: String,
}

/// What the header of an RPM says about itself.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct RpmMetadata {
    pub name: String,
    pub version: String,
    pub release: String,
    pub epoch: Option<String>,
    pub architecture: String,
    pub summary: String,
    pub description: String,
    pub url: String,
    pub license: String,
    pub packager: String,
    /// Requirements a pacman install could not satisfy by name.
    pub requires: Vec<String>,
    pub provides: Vec<String>,
    pub scriptlets: Vec<Scriptlet>,
    /// The scriptlet phases that ask for an interpreter other than a shell.
    pub payload_format: String,
    pub payload_compressor: String,
    pub file_count: usize,
}

/// One index entry, with its raw fields kept because the type decides how the
/// data store is read.
struct Entry {
    tag: u32,
    atype: u32,
    offset: u32,
    count: u32,
}

/// A header region: its index and the data store the index points into.
struct Header<'a> {
    entries: Vec<Entry>,
    data: &'a [u8],
}

impl<'a> Header<'a> {
    /// Read the region that starts at `at`, returning it and the offset just
    /// past its end.
    fn parse(bytes: &'a [u8], at: usize) -> Result<(Header<'a>, usize), String> {
        let blob = bytes
            .get(at..)
            .ok_or_else(|| format!("RPM header starts past the end of the file ({at} bytes in)"))?;
        if blob.len() < HEADER_SIZE {
            return Err("RPM header is truncated before its 16-byte prefix".to_string());
        }
        if blob[0..3] != RPM_MAGIC {
            return Err(format!(
                "RPM header does not start with 0x8eade8 (found {:02x}{:02x}{:02x})",
                blob[0], blob[1], blob[2]
            ));
        }
        if blob[3] != 1 {
            return Err(format!("Unsupported RPM header version {}", blob[3]));
        }
        // The prefix is `magic, version, reserved, indexCount, dataLen` — all
        // 32-bit big endian. (Signature headers in the wild carry a 16-byte
        // region trailer at the end of their data store, which is read here as
        // just another entry and then ignored.)
        let count = be_u32(blob, 8).ok_or("RPM header prefix is truncated")?;
        let data_len = be_u32(blob, 12).ok_or("RPM header prefix is truncated")?;
        if count > MAX_INDEX_ENTRIES {
            return Err(format!(
                "RPM header claims {count} index entries, above the {MAX_INDEX_ENTRIES} ceiling"
            ));
        }
        let index_bytes = usize::try_from(count)
            .ok()
            .and_then(|count| count.checked_mul(INDEX_ENTRY_SIZE))
            .ok_or("RPM header index size overflows")?;
        let data_len = usize::try_from(data_len).map_err(|_| "RPM header data length is absurd")?;
        let region = HEADER_SIZE
            .checked_add(index_bytes)
            .and_then(|value| value.checked_add(data_len))
            .ok_or("RPM header region size overflows")?;
        if region > blob.len() {
            return Err(format!(
                "RPM header claims a {region} byte region but only {} bytes follow it",
                blob.len()
            ));
        }

        let raw = &blob[HEADER_SIZE..HEADER_SIZE + index_bytes];
        let data = &blob[HEADER_SIZE + index_bytes..region];
        let mut entries = Vec::with_capacity(count as usize);
        let mut position = 0usize;
        let mut cursor = 0usize;
        while cursor + INDEX_ENTRY_SIZE <= raw.len() {
            let chunk = &raw[cursor..cursor + INDEX_ENTRY_SIZE];
            let entry = Entry {
                tag: be_u32(chunk, 0).ok_or("truncated index entry")?,
                atype: be_u32(chunk, 4).ok_or("truncated index entry")?,
                offset: be_u32(chunk, 8).ok_or("truncated index entry")?,
                count: be_u32(chunk, 12).ok_or("truncated index entry")?,
            };
            entry.validate(data.len()).map_err(|reason| {
                format!(
                    "RPM header index entry {} (tag {}, type {}): {}",
                    position, entry.tag, entry.atype, reason
                )
            })?;
            entries.push(entry);
            position += 1;
            cursor += INDEX_ENTRY_SIZE;
        }
        Ok((Header { entries, data }, at + region))
    }

    fn get(&self, tag: u32) -> Option<&Entry> {
        self.entries.iter().find(|entry| entry.tag == tag)
    }

    fn string(&self, tag: u32) -> Option<String> {
        let entry = self.get(tag)?;
        if !matches!(
            entry.atype,
            TYPE_STRING | TYPE_STRING_ARRAY | TYPE_I18N_STRING
        ) {
            return None;
        }
        let start = usize::try_from(entry.offset).ok()?;
        self.data.get(start..).and_then(read_str)
    }

    fn strings(&self, tag: u32) -> Vec<String> {
        let Some(entry) = self.get(tag) else {
            return Vec::new();
        };
        if !matches!(
            entry.atype,
            TYPE_STRING_ARRAY | TYPE_I18N_STRING | TYPE_STRING
        ) {
            return Vec::new();
        }
        let Some(start) = usize::try_from(entry.offset).ok() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        let mut cursor = start;
        // The declared count is capped and the walk stops at the first string
        // that is not terminated inside the data store, so a hostile count
        // cannot read past the end or fill memory.
        let limit = entry.count.min(MAX_ARRAY_ITEMS) as usize;
        while out.len() < limit {
            let rest = match self.data.get(cursor..) {
                Some(rest) => rest,
                None => break,
            };
            let Some(value) = read_str(rest) else {
                break;
            };
            cursor = match cursor.checked_add(value.len() + 1) {
                Some(next) => next,
                None => break,
            };
            out.push(value);
        }
        out
    }

    fn ints(&self, tag: u32) -> Vec<i64> {
        let Some(entry) = self.get(tag) else {
            return Vec::new();
        };
        let width = match entry.atype {
            TYPE_INT8 | TYPE_CHAR => 1,
            TYPE_INT16 => 2,
            TYPE_INT32 => 4,
            TYPE_INT64 => 8,
            _ => return Vec::new(),
        };
        let Some(start) = usize::try_from(entry.offset).ok() else {
            return Vec::new();
        };
        let limit = entry.count.min(MAX_ARRAY_ITEMS) as usize;
        let mut out = Vec::new();
        for index in 0..limit {
            let at = match start.checked_add(index.saturating_mul(width)) {
                Some(at) => at,
                None => break,
            };
            let slice = match self.window(at, width) {
                Some(slice) => slice,
                None => break,
            };
            let value = match width {
                1 => Some(i64::from(slice[0] as i8)),
                2 => be_u16(slice).map(|raw| i64::from(raw as i16)),
                4 => be_u32(slice, 0).map(|raw| raw as i32 as i64),
                8 => slice
                    .try_into()
                    .ok()
                    .map(|bytes: [u8; 8]| u64::from_be_bytes(bytes) as i64),
                _ => None,
            };
            match value {
                Some(value) => out.push(value),
                None => break,
            }
        }
        out
    }

    /// A window of the data store, refused when it would run past the end.
    fn window(&self, at: usize, width: usize) -> Option<&'a [u8]> {
        let end = at.checked_add(width)?;
        self.data.get(at..end)
    }
}

impl Entry {
    /// Assert the entry describes something that fits inside the data store.
    fn validate(&self, data_len: usize) -> Result<(), &'static str> {
        let width = match self.atype {
            TYPE_CHAR | TYPE_INT8 => 1,
            TYPE_INT16 => 2,
            TYPE_INT32 => 4,
            TYPE_INT64 => 8,
            TYPE_STRING | TYPE_BIN | TYPE_STRING_ARRAY | TYPE_I18N_STRING => 1,
            _ => return Err("unknown tag type"),
        };
        let offset = usize::try_from(self.offset).map_err(|_| "offset is out of range")?;
        if offset > data_len {
            return Err("offset points past the data store");
        }
        if self.count > MAX_ARRAY_ITEMS {
            return Err("item count is above the ceiling");
        }
        let span = usize::try_from(self.count)
            .map_err(|_| "item count is out of range")?
            .checked_mul(width)
            .ok_or("item span overflows")?;
        if offset.checked_add(span).ok_or("item span overflows")? > data_len {
            return Err("item span runs past the data store");
        }
        Ok(())
    }
}

/// Decode the lead, the signature header and the main header of an RPM file.
pub fn decode(bytes: &[u8]) -> Result<RpmMetadata, String> {
    if bytes.len() < LEAD_SIZE {
        return Err(format!(
            "Truncated RPM: {} bytes is shorter than the 96-byte lead",
            bytes.len()
        ));
    }
    if bytes[0..2] != LEAD_MAGIC {
        return Err(format!(
            "Not an RPM file: the lead starts with {:02x}{:02x}, not edab",
            bytes[0], bytes[1]
        ));
    }
    let (_, after_signature) = Header::parse(bytes, LEAD_SIZE)?;
    // rpm aligns the header that follows the signatures to an 8-byte boundary;
    // the lead is 96 bytes, so both readings of "aligned" agree.
    let main_at = (after_signature + 7) & !7;
    if main_at >= bytes.len() {
        return Err(format!(
            "Truncated RPM: the main header would start at byte {main_at}, but the file is {} bytes",
            bytes.len()
        ));
    }
    let (header, _) = Header::parse(bytes, main_at)?;

    let epoch = header
        .get(TAG_EPOCH)
        .filter(|entry| entry.count > 0)
        .and_then(|_| header.ints(TAG_EPOCH).first().copied())
        .map(|value| value.to_string());

    let mut metadata = RpmMetadata {
        name: header.string(TAG_NAME).unwrap_or_default(),
        version: header.string(TAG_VERSION).unwrap_or_default(),
        release: header.string(TAG_RELEASE).unwrap_or_default(),
        epoch,
        architecture: header.string(TAG_ARCH).unwrap_or_default(),
        summary: header.string(TAG_SUMMARY).unwrap_or_default(),
        description: header.string(TAG_DESCRIPTION).unwrap_or_default(),
        url: header.string(TAG_URL).unwrap_or_default(),
        license: header.string(TAG_LICENSE).unwrap_or_default(),
        packager: header.string(TAG_PACKAGER).unwrap_or_default(),
        requires: dependency_list(
            header.strings(TAG_REQUIRENAME),
            header.ints(TAG_REQUIREFLAGS),
            header.strings(TAG_REQUIREVERSION),
        ),
        provides: header.strings(TAG_PROVIDENAME),
        scriptlets: scriptlets(&header),
        payload_format: header
            .string(TAG_PAYLOADFORMAT)
            .unwrap_or_else(|| "cpio".to_string()),
        payload_compressor: header
            .string(TAG_PAYLOADCOMPRESSOR)
            .unwrap_or_else(|| "unknown".to_string()),
        file_count: header.strings(TAG_BASENAMES).len(),
    };
    if metadata.name.is_empty() {
        return Err("RPM header carries no NAME tag".to_string());
    }
    if metadata.summary.is_empty() && !metadata.description.is_empty() {
        metadata.summary = metadata
            .description
            .lines()
            .next()
            .unwrap_or("")
            .to_string();
    }
    Ok(metadata)
}

/// The six scriptlet phases rpm knows, each with the tag holding its
/// interpreter. Order is the order the installer would run them in.
const SCRIPTLET_TAGS: [(u32, u32, &str); 6] = [
    (TAG_PREIN, TAG_PREINPROG, "prein"),
    (TAG_POSTIN, TAG_POSTINPROG, "postin"),
    (TAG_PREUN, TAG_PREUNPROG, "preun"),
    (TAG_POSTUN, TAG_POSTUNPROG, "postun"),
    (TAG_PRETRANS, TAG_PRETRANSPROG, "pretrans"),
    (TAG_POSTTRANS, TAG_POSTTRANSPROG, "posttrans"),
];

fn scriptlets(header: &Header) -> Vec<Scriptlet> {
    let mut out = Vec::new();
    for (body_tag, prog_tag, name) in SCRIPTLET_TAGS {
        let Some(body) = header.string(body_tag) else {
            continue;
        };
        if body.trim().is_empty() {
            // A phase declared with nothing to run is not a scriptlet to review.
            continue;
        }
        let interpreter = header
            .get(prog_tag)
            .and_then(|entry| {
                let start = usize::try_from(entry.offset).ok()?;
                header.data.get(start..).and_then(read_str)
            })
            .filter(|value| !value.is_empty());
        out.push(Scriptlet {
            name: name.to_string(),
            interpreter,
            body,
        });
    }
    out
}

/// rpm's `Requires` as readable names. The two kinds of dependency a native
/// install can do nothing about are dropped: `rpmlib(...)` features are
/// properties of the packaging tool, and `config(...)` entries name a file the
/// package owns rather than another package.
fn dependency_list(names: Vec<String>, flags: Vec<i64>, versions: Vec<String>) -> Vec<String> {
    let mut out = Vec::new();
    for (index, name) in names.into_iter().enumerate() {
        let flag = flags.get(index).copied().unwrap_or(0) as i32;
        if flag & SENSE_RPMLIB != 0 || name.starts_with("rpmlib(") {
            continue;
        }
        if flag & SENSE_CONFIG != 0 || name.starts_with("config(") {
            continue;
        }
        let sense = flag & SENSE_MASK;
        let version = versions.get(index).map(|value| value.trim()).unwrap_or("");
        if sense == 0 || version.is_empty() {
            out.push(name);
            continue;
        }
        let operator = match (
            sense & SENSE_LESS != 0,
            sense & SENSE_GREATER != 0,
            sense & SENSE_EQUAL != 0,
        ) {
            (false, false, true) => "=",
            (true, false, true) => "<=",
            (false, true, true) => ">=",
            (true, false, false) => "<",
            (false, true, false) => ">",
            (true, true, false) => "!=",
            _ => "any of",
        };
        out.push(format!("{name} ({operator} {version})"));
    }
    out
}

fn read_str(bytes: &[u8]) -> Option<String> {
    let limit = bytes.len().min(MAX_STRING_BYTES);
    let window = &bytes[..limit];
    let terminator = window.iter().position(|byte| *byte == 0)?;
    let value = std::str::from_utf8(&window[..terminator]).ok()?;
    Some(value.to_string())
}

fn be_u16(bytes: &[u8]) -> Option<u16> {
    let slice: &[u8; 2] = bytes.get(0..2)?.try_into().ok()?;
    Some(u16::from_be_bytes(*slice))
}

fn be_u32(bytes: &[u8], at: usize) -> Option<u32> {
    let slice: &[u8; 4] = bytes.get(at..at.checked_add(4)?)?.try_into().ok()?;
    Some(u32::from_be_bytes(*slice))
}

/// One tag to place in the main header of a fixture package.
#[derive(Debug, Clone)]
pub enum FixtureTag {
    Text(u32, String),
    TextList(u32, Vec<String>),
    Int32s(u32, Vec<i32>),
}

impl FixtureTag {
    pub fn text(tag: u32, value: impl Into<String>) -> Self {
        FixtureTag::Text(tag, value.into())
    }
    pub fn list(tag: u32, values: &[&str]) -> Self {
        FixtureTag::TextList(
            tag,
            values.iter().map(|value| (*value).to_string()).collect(),
        )
    }
    pub fn ints(tag: u32, values: &[i32]) -> Self {
        FixtureTag::Int32s(tag, values.to_vec())
    }
}

/// Assembles a complete RPM image — the 96-byte lead, a signature header with
/// the padding rpm puts in front of the next region, a main header, and no
/// payload — from the tags a caller supplies. Product code only ever reads such
/// an image; it exists so a host with no rpm tooling can still produce a real
/// package to inspect.
pub fn build_fixture_package(tags: &[FixtureTag]) -> Vec<u8> {
    let mut index = Vec::new();
    let mut store = Vec::new();
    for tag in tags {
        append_tag(&mut index, &mut store, tag);
    }
    let main = region(&index, &store);

    let mut sig_index = Vec::new();
    let mut sig_store = Vec::new();
    append_tag(
        &mut sig_index,
        &mut sig_store,
        &FixtureTag::Int32s(1000, vec![main.len() as i32]),
    );
    let signature = region(&sig_index, &sig_store);

    let mut lead = vec![0u8; LEAD_SIZE];
    lead[0..4].copy_from_slice(&[0xed, 0xab, 0xee, 0xdb]);
    lead[4] = 3; // lead major version
    lead[6..8].copy_from_slice(&0u16.to_be_bytes()); // binary package
    lead[76..78].copy_from_slice(&1u16.to_be_bytes()); // osnum
    lead[78..80].copy_from_slice(&5u16.to_be_bytes()); // signature type

    let mut out = Vec::with_capacity(lead.len() + signature.len() + main.len() + 8);
    out.extend_from_slice(&lead);
    out.extend_from_slice(&signature);
    // rpm aligns the header after the signatures to an 8-byte boundary, and this
    // fixture's signature region is deliberately not a multiple of 8.
    while out.len() % 8 != 0 {
        out.push(0);
    }
    out.extend_from_slice(&main);
    out
}

fn append_tag(index: &mut Vec<u8>, store: &mut Vec<u8>, tag: &FixtureTag) {
    let (tag_id, atype, count, alignment) = match tag {
        FixtureTag::Text(id, _) => (*id, TYPE_STRING, 1, 1),
        FixtureTag::TextList(id, values) => (*id, TYPE_STRING_ARRAY, values.len(), 1),
        FixtureTag::Int32s(id, values) => (*id, TYPE_INT32, values.len(), 4),
    };
    // rpm aligns each type's data on its own width; the fixture has to write the
    // same padding a real build would, or the reader is only ever tested against
    // offsets that happen to line up.
    let alignment = alignment.max(1);
    let pad = alignment - store.len() % alignment;
    if pad != alignment {
        store.resize(store.len() + pad, 0);
    }
    let offset = store.len() as u32;
    match tag {
        FixtureTag::Text(_, value) => {
            store.extend_from_slice(value.as_bytes());
            store.push(0);
        }
        FixtureTag::TextList(_, values) => {
            for value in values {
                store.extend_from_slice(value.as_bytes());
                store.push(0);
            }
        }
        FixtureTag::Int32s(_, values) => {
            for value in values {
                store.extend_from_slice(&(*value as u32).to_be_bytes());
            }
        }
    }
    index.extend_from_slice(&tag_id.to_be_bytes());
    index.extend_from_slice(&atype.to_be_bytes());
    index.extend_from_slice(&offset.to_be_bytes());
    index.extend_from_slice(&(count as u32).to_be_bytes());
}

fn region(index: &[u8], store: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(HEADER_SIZE + index.len() + store.len());
    out.extend_from_slice(&RPM_MAGIC);
    out.push(1); // header major version
    out.extend_from_slice(&[0u8; 4]); // reserved
    out.extend_from_slice(&((index.len() / INDEX_ENTRY_SIZE) as u32).to_be_bytes());
    out.extend_from_slice(&(store.len() as u32).to_be_bytes());
    out.extend_from_slice(index);
    out.extend_from_slice(store);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tags() -> Vec<FixtureTag> {
        vec![
            FixtureTag::text(TAG_NAME, "hello"),
            FixtureTag::text(TAG_VERSION, "2.10"),
            FixtureTag::text(TAG_RELEASE, "3"),
            FixtureTag::ints(TAG_EPOCH, &[7]),
            FixtureTag::text(TAG_ARCH, "x86_64"),
            FixtureTag::text(TAG_SUMMARY, "A friendly greeter"),
            FixtureTag::text(
                TAG_DESCRIPTION,
                "A friendly greeter.\nWrites greetings on demand.",
            ),
            FixtureTag::text(TAG_LICENSE, "GPLv3+"),
            FixtureTag::text(TAG_URL, "https://example.test/hello"),
            FixtureTag::text(TAG_PACKAGER, "Packager <p@example.test>"),
            FixtureTag::list(
                TAG_REQUIRENAME,
                &[
                    "rpmlib(CompressedFileNames)",
                    "config(/etc/hello.conf)",
                    "glibc",
                    "libfoo",
                    "bash",
                ],
            ),
            FixtureTag::ints(
                TAG_REQUIREFLAGS,
                &[
                    SENSE_RPMLIB,
                    SENSE_CONFIG,
                    SENSE_EQUAL,
                    SENSE_GREATER | SENSE_EQUAL,
                    0,
                ],
            ),
            FixtureTag::list(TAG_REQUIREVERSION, &["3.0.4", "2.17", "2.34", "1.2", ""]),
            FixtureTag::list(TAG_PROVIDENAME, &["hello", "hello-doc"]),
            FixtureTag::list(TAG_BASENAMES, &["hello", "hello.1.gz", "hello.conf"]),
            FixtureTag::text(TAG_PAYLOADFORMAT, "cpio"),
            FixtureTag::text(TAG_PAYLOADCOMPRESSOR, "zstd"),
            FixtureTag::text(TAG_PREIN, "#!/bin/sh\necho preinst\n"),
            FixtureTag::text(TAG_POSTIN, "/usr/bin/mkdir -p /var/lib/hello\n"),
            FixtureTag::text(TAG_PREUN, "rm -f /var/lib/hello/state\n"),
            FixtureTag::text(TAG_POSTUN, ""),
            FixtureTag::text(TAG_PRETRANS, "echo pretrans\n"),
            FixtureTag::text(TAG_POSTTRANS, "echo posttrans\n"),
            // %post runs under an interpreter other than the shell.
            FixtureTag::text(TAG_POSTINPROG, "/usr/bin/lua"),
        ]
    }

    fn decoded() -> RpmMetadata {
        decode(&build_fixture_package(&tags())).expect("fixture should decode")
    }

    #[test]
    fn the_identifying_tags_come_out_of_the_header() {
        let metadata = decoded();
        assert_eq!(metadata.name, "hello");
        assert_eq!(metadata.version, "2.10");
        assert_eq!(metadata.release, "3");
        assert_eq!(metadata.epoch.as_deref(), Some("7"));
        assert_eq!(metadata.architecture, "x86_64");
        assert_eq!(metadata.summary, "A friendly greeter");
        assert!(metadata.description.contains("on demand"));
        assert_eq!(metadata.license, "GPLv3+");
        assert_eq!(metadata.url, "https://example.test/hello");
        assert_eq!(metadata.packager, "Packager <p@example.test>");
        assert_eq!(metadata.payload_format, "cpio");
        assert_eq!(metadata.payload_compressor, "zstd");
        assert_eq!(metadata.file_count, 3);
    }

    #[test]
    fn requirements_keep_named_packages_and_drop_packaging_internals() {
        let metadata = decoded();
        assert_eq!(
            metadata.requires,
            vec![
                "glibc (= 2.34)".to_string(),
                "libfoo (>= 1.2)".to_string(),
                "bash".to_string(),
            ],
            "rpmlib() capabilities and config() files are not packages"
        );
        assert_eq!(metadata.provides, vec!["hello", "hello-doc"]);
    }

    #[test]
    fn scriptlets_are_read_in_transaction_order_with_their_interpreters() {
        let scriptlets = decoded().scriptlets;
        let names: Vec<&str> = scriptlets
            .iter()
            .map(|script| script.name.as_str())
            .collect();
        assert_eq!(names, ["prein", "postin", "preun", "pretrans", "posttrans"]);
        assert!(
            !scriptlets
                .iter()
                .any(|script| script.name == "postun" && !script.body.is_empty()),
            "an empty %postun is no scriptlet to review"
        );
        assert!(scriptlets[0].body.contains("preinst"));
        assert_eq!(scriptlets[1].interpreter.as_deref(), Some("/usr/bin/lua"));
        assert_eq!(scriptlets[0].interpreter, None);
    }

    #[test]
    fn a_file_that_is_not_an_rpm_is_refused() {
        assert!(decode(b"").is_err());
        assert!(decode(b"not even close to an rpm lead").is_err());
        let mut bytes = build_fixture_package(&tags());
        bytes[0] = b'X';
        assert!(decode(&bytes).unwrap_err().contains("Not an RPM file"));
        // Shorter than the lead.
        assert!(decode(&bytes[..90]).is_err());
    }

    #[test]
    fn counters_larger_than_the_file_are_refused_not_followed() {
        let good = build_fixture_package(&tags());
        // The main header starts after the lead, the signature region and its
        // padding; rewrite its index count as something the file cannot hold.
        let main_at = good.len() - region_len(&good);
        let mut bytes = good.clone();
        bytes[main_at + 8..main_at + 12].copy_from_slice(&0xFFFF_FFF0u32.to_be_bytes());
        assert!(decode(&bytes).is_err());

        // And its data length.
        let mut bytes = good.clone();
        bytes[main_at + 12..main_at + 16].copy_from_slice(&u32::MAX.to_be_bytes());
        assert!(decode(&bytes).is_err());

        // A header whose region runs past the end of the file.
        let mut bytes = good.clone();
        bytes.truncate(main_at + 40);
        assert!(decode(&bytes).is_err());
    }

    #[test]
    fn index_entries_outside_the_data_store_are_refused() {
        let good = build_fixture_package(&tags());
        let main_at = good.len() - region_len(&good);
        let mut bytes = good.clone();
        // First index entry, offset field.
        bytes[main_at + HEADER_SIZE + 8..main_at + HEADER_SIZE + 12]
            .copy_from_slice(&0xFFFF_F000u32.to_be_bytes());
        let error = decode(&bytes).unwrap_err();
        assert!(error.contains("data store"), "{error}");
    }

    #[test]
    fn an_unknown_tag_type_is_refused() {
        let good = build_fixture_package(&tags());
        let main_at = good.len() - region_len(&good);
        let mut bytes = good.clone();
        bytes[main_at + HEADER_SIZE + 4..main_at + HEADER_SIZE + 8]
            .copy_from_slice(&99u32.to_be_bytes());
        assert!(decode(&bytes).is_err());
    }

    #[test]
    fn truncating_a_package_anywhere_never_panics() {
        let good = build_fixture_package(&tags());
        for cut in (LEAD_SIZE..good.len()).step_by(7) {
            let _ = decode(&good[..cut]);
        }
        // Corrupting single bytes of a valid image must fail or succeed, never
        // hang or abort.
        for index in LEAD_SIZE..good.len() {
            let mut bytes = good.clone();
            bytes[index] = bytes[index].wrapping_add(0x37);
            let _ = decode(&bytes);
        }
    }

    /// Byte length of the main header region at the end of a fixture image.
    fn region_len(bytes: &[u8]) -> usize {
        let at = main_header_at(bytes);
        let count = be_u32(bytes, at + 8).unwrap() as usize;
        let data_len = be_u32(bytes, at + 12).unwrap() as usize;
        HEADER_SIZE + count * INDEX_ENTRY_SIZE + data_len
    }

    fn main_header_at(bytes: &[u8]) -> usize {
        let mut at = LEAD_SIZE;
        loop {
            let count = be_u32(bytes, at + 8).unwrap() as usize;
            let data_len = be_u32(bytes, at + 12).unwrap() as usize;
            let end = HEADER_SIZE + count * INDEX_ENTRY_SIZE + data_len + at;
            let next = (end + 7) & !7;
            if next >= bytes.len() {
                return at;
            }
            at = next;
        }
    }

    #[test]
    fn the_fixture_image_is_the_shape_the_reader_expects() {
        let bytes = build_fixture_package(&tags());
        assert!(bytes.len() > LEAD_SIZE + 2 * HEADER_SIZE);
        assert_eq!(bytes[0..2], LEAD_MAGIC[..]);
        let main_at = main_header_at(&bytes);
        assert_eq!(main_at % 8, 0, "headers are 8-byte aligned");
        assert_ne!(
            main_at, LEAD_SIZE,
            "the signature region should need padding"
        );
    }
}
