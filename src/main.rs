use archbridge::engine::Plan;
use archbridge::rpc::RpcServer;
use serde_json::json;
use std::env;
use std::io::{self, BufReader, Write};

/// Write to stdout without panicking. `println!` panics when stdout is closed
/// (`archbridge search foo | head -0`) or full (`> /dev/full`), turning a
/// routine pipe into exit 101 and a backtrace hint. A closed pipe is the
/// reader's choice and ends quietly; any other write error is reported once.
fn write_stdout(args: std::fmt::Arguments<'_>, newline: bool) {
    let mut out = io::stdout().lock();
    let result = out
        .write_fmt(args)
        .and_then(|_| {
            if newline {
                out.write_all(b"\n")
            } else {
                Ok(())
            }
        })
        .and_then(|_| out.flush());
    if let Err(err) = result {
        if err.kind() == io::ErrorKind::BrokenPipe {
            std::process::exit(0);
        }
        eprintln!("Error: failed writing to stdout: {}", err);
        std::process::exit(1);
    }
}

macro_rules! print {
    ($($arg:tt)*) => { write_stdout(format_args!($($arg)*), false) };
}

macro_rules! println {
    ($($arg:tt)*) => { write_stdout(format_args!($($arg)*), true) };
}

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        eprint!("{}", USAGE);
        std::process::exit(2);
    }

    let command = args[1].as_str();
    if command == "help" || command == "--help" || command == "-h" {
        print!("{}", USAGE);
        std::process::exit(0);
    }

    if command == "gui" || command == "--gui" || command == "ui" || command == "--ui" {
        if wants_help(&args[2..]) {
            print!("{}", command_help("gui"));
            std::process::exit(0);
        }
        // Launch the native GPUI window — no Python subprocess needed
        archbridge::gui::run_gui();
        return;
    }

    if command == "serve" {
        let code = match handle_serve(&args[2..]) {
            Ok(code) => code,
            Err(err) => {
                eprintln!("RPC server error: {}", err);
                1
            }
        };
        std::process::exit(code);
    }

    let mut client = DirectRpcClient::new();
    let res = match command {
        "search" | "info" => handle_search(&mut client, &args[2..]),
        "inspect" => handle_inspect(&mut client, &args[2..]),
        "build" => handle_build(&mut client, &args[2..]),
        "install" => handle_install(&mut client, &args[2..]),
        "uninstall" | "remove" => handle_uninstall(&mut client, &args[2..]),
        "test" => handle_test(&mut client, &args[2..]),
        "config" => handle_config(&mut client, &args[2..]),
        "doctor" => handle_doctor(&mut client, &args[2..]),
        _ => {
            eprintln!("Unknown command: '{}'", command);
            eprint!("{}", USAGE);
            Ok(2)
        }
    };

    match res {
        Ok(code) => std::process::exit(code),
        Err(err) => {
            eprintln!("Error: {}", err);
            std::process::exit(1);
        }
    }
}

/// Help the user asked for goes to stdout; the same text printed because
/// something was wrong goes to stderr, so a failing run never contaminates a
/// pipe and `archbridge --help` works with a pager.
const USAGE: &str = "\
ArchBridge — software discovery and packaging assistant for Arch Linux

USAGE:
  archbridge search <name> [--repo <url>]
  archbridge info <name>
  archbridge inspect <file.deb|file.rpm>
  archbridge build <url|directory|PKGBUILD> [--name <name>] [--version <ver>] [--entry <bin>] [--dependency <pkg>...] [--smoke-arg <arg>...] [--dry-run] [--yes]
  archbridge install <name|package-file|source-url> [--dry-run] [--yes]
  archbridge uninstall <installed-package-name> [--dry-run] [--yes]
  archbridge test <package.pkg.tar.zst> [--entry <bin>] [--smoke-arg <arg>...] [--dry-run] [--yes]
  archbridge config <get|set> <key> [value] [--yes]
  archbridge doctor
  archbridge serve
  archbridge gui

Run 'archbridge <command> --help' for one command's flags.

EXIT CODES:
  0  success, or help that was asked for
  1  operational error (engine, build, install, or smoke-test failure)
  2  invalid arguments or unknown command
  3  plan blocked, or the user declined to confirm
";

/// `--help` beats every other argument: someone asking how a command runs never
/// wants that command to run, and used to want `doctor --help` to say so.
fn wants_help(args: &[String]) -> bool {
    args.iter().any(|arg| arg == "--help" || arg == "-h")
}

fn command_help(command: &str) -> &'static str {
    match command {
        "search" | "info" => {
            "\
archbridge search <name> [--repo <url>]
archbridge info <name>

Query every enabled source (official repositories, AUR, Flatpak, AppImage,
upstream, DEB, RPM) and print the decision table with the recommended source.
Read-only: nothing is downloaded or installed.

FLAGS:
  --repo <url>   also consider this repository URL for the target
  -h, --help     show this help and exit 0

EXIT CODES:
  0  the query ran   1  the query failed   2  bad arguments

EXAMPLES:
  archbridge search htop
  archbridge search htop --repo https://github.com/htop-dev/htop
"
        }
        "inspect" => {
            "\
archbridge inspect <file.deb|file.rpm>

Parse a foreign package statically: metadata, declared dependencies, shared
libraries, payload file lists, systemd units and maintainer scripts.
Data only — scriptlets are decoded for review and NEVER executed.

FLAGS:
  -h, --help     show this help and exit 0

EXIT CODES:
  0  inspected   1  the file cannot be parsed   2  bad arguments
"
        }
        "build" => {
            "\
archbridge build <url|directory|PKGBUILD> [options]

Stage an immutable, hashed build plan for a source tree, an HTTPS Git host, a
PKGBUILD, or a foreign .deb/.rpm payload (repacked as an Arch package), then
execute it after confirmation. The plan names the builder and isolation that
will be used: an unprivileged host build is attempted first and an isolated
clean chroot is the fallback.

FLAGS:
  --name <name>             package name (defaults to the target's file name)
  --version <ver>           package version
  --entry <bin>             entry binary used by the runtime smoke test
  --dependency <pkg>        add a build/runtime dependency (repeatable)
  --smoke-arg <arg>         argument passed to the entry binary in the smoke
                            test instead of the default --version (repeatable)
  --dry-run                 print the plan and stop; nothing is executed
  --yes, -y                 skip the interactive confirmation prompt
  -h, --help                show this help and exit 0

EXIT CODES:
  0  plan printed, or build and smoke test passed
  1  build or smoke test failed
  2  bad arguments
  3  plan blocked, or confirmation declined
"
        }
        "install" => {
            "\
archbridge install <name|package-file|source-url> [--dry-run] [--yes]

Install a local Arch package (.pkg.tar.*), repack and install a foreign
.deb/.rpm, build-then-install an HTTPS GitHub/GitLab source, or resolve a bare
package name through discovery. A name is only installable when discovery
recommends a source with an install command ('official'); otherwise ArchBridge
reports what each source answered and points at 'search' or 'build' instead of
guessing.

FLAGS:
  --dry-run                print the plan and stop; nothing is installed
  --yes, -y                skip the interactive confirmation prompt
  -h, --help               show this help and exit 0

EXIT CODES:
  0  plan printed, or installation succeeded
  1  installation failed, or the name could not be resolved
  2  bad arguments   3  plan blocked, or confirmation declined

EXAMPLES:
  archbridge install htop --dry-run
  archbridge install ./app-1.0-1-x86_64.rpm
"
        }
        "uninstall" | "remove" => {
            "\
archbridge uninstall <installed-package-name> [--dry-run] [--yes]

Remove an installed Arch package and its now-unused dependencies with
'pacman -Rns'. Takes a package name, never a path or a file.

FLAGS:
  --dry-run      print the plan and stop; nothing is removed
  --yes, -y      skip the interactive confirmation prompt
  -h, --help     show this help and exit 0

EXIT CODES:
  0  plan printed, or removal succeeded   1  removal failed
  2  bad arguments   3  plan blocked, or confirmation declined
"
        }
        "test" => {
            "\
archbridge test <package.pkg.tar.zst> [--entry <bin>] [--smoke-arg <arg>...] [--dry-run] [--yes]

Run the package's entry binary inside an isolated systemd-nspawn container
(unprivileged user namespace, no network, UID 65534, 30-second timeout).

With --dry-run this prints the plan and NOTHING IS EXECUTED; the output says so
explicitly. Without --dry-run the plan is reviewed, confirmed and then really
run, and the exit code reflects the container's result.

FLAGS:
  --entry <bin>       entry binary to launch (defaults to the package name)
  --smoke-arg <arg>   argument for the entry binary instead of --version
                      (repeatable)
  --dry-run           plan only; nothing is executed
  --yes, -y           skip the interactive confirmation prompt
  -h, --help          show this help and exit 0

EXIT CODES:
  0  plan printed, or the smoke test passed
  1  the smoke test failed or could not run
  2  bad arguments   3  plan blocked, or confirmation declined
"
        }
        "config" => {
            "\
archbridge config <get|set> <key> [value] [--yes]

Read or change the persisted preferences in
~/.config/archbridge/config.json. Changing a preference invalidates every
pending plan, because those plans were reviewed under the old settings.

SUBCOMMANDS:
  get [key]           print one key, or every key when omitted
  set <key> <value>   stage a plan for the change, then apply it

KEYS:
  official, aur, flatpak, appimage, upstream, deb, rpm   (boolean)
  output_dir                            absolute path, or empty for the job dir
  repo.<name>                           a named repository URL

FLAGS:
  --dry-run      print the plan and stop; the change is not written
  --yes, -y      skip the interactive confirmation prompt
  -h, --help     show this help and exit 0

EXIT CODES:
  0  read, plan printed, or change applied   1  the change failed
  2  bad arguments   3  plan blocked, or confirmation declined
"
        }
        "doctor" => {
            "\
archbridge doctor

Check the packaging prerequisites ArchBridge needs: whether this is an Arch
Linux host at all, then pacman, base-devel, devtools, compiler toolchain,
keyring, user namespaces, workspace disk space and an HTTPS reachability probe.

'ready: false' is reported with the failing check and its message; on a host
without pacman and the Arch build tools the first reason is that this is not Arch
Linux. An offline network probe reads 'unavailable' and does NOT by itself make
the system unready, because local builds and foreign-package inspection work
offline.

FLAGS:
  -h, --help     show this help and exit 0

EXIT CODES:
  0  ready: true   1  ready: false   2  unexpected arguments
"
        }
        "serve" => {
            "\
archbridge serve

Run the long-lived JSON-RPC 2.0 server: one request per line on stdin, one
response per line on stdout. Exit 2 when asked for more than that.

FLAGS:
  -h, --help     show this help and exit 0

EXIT CODES:
  0  stdin reached EOF   1  the server loop failed   2  unexpected arguments
"
        }
        "gui" => {
            "\
archbridge gui        (aliases: ui, --gui, --ui)

Launch the native GPUI desktop application. It talks to the same in-process
engine as the CLI, so plans still require explicit confirmation before
anything is executed.

FLAGS:
  -h, --help     show this help and exit 0
"
        }
        _ => USAGE,
    }
}

pub struct DirectRpcClient {
    server: RpcServer,
}

impl Default for DirectRpcClient {
    fn default() -> Self {
        Self::new()
    }
}

impl DirectRpcClient {
    pub fn new() -> Self {
        Self {
            server: RpcServer::new(),
        }
    }

    pub fn call(
        &mut self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        let req = archbridge::rpc::JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(json!(1)),
            method: method.to_string(),
            params,
        };
        let resp = self.server.handle_request(req);
        if let Some(err) = resp.error {
            Err(format!("RPC error {}: {}", err.code, err.message))
        } else if let Some(res) = resp.result {
            Ok(res)
        } else {
            Err("Empty RPC response".to_string())
        }
    }
}

/// Flags every single-target command understands. Anything else is reported by
/// the caller: an ignored `--dryrun` silently meant a real run.
fn parse_flags(
    args: &[String],
    problems: &mut Vec<String>,
) -> (Vec<String>, bool, bool, Option<String>) {
    let mut positional = Vec::new();
    let mut dry_run = false;
    let mut yes = false;
    let mut repo = None;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            // End of options: everything after is a positional, so a target or a
            // value that begins with '-' can still be passed.
            "--" => {
                positional.extend(args[i + 1..].iter().cloned());
                break;
            }
            "--dry-run" => dry_run = true,
            "--yes" | "-y" => yes = true,
            "--repo" => {
                if i + 1 < args.len() {
                    repo = Some(args[i + 1].clone());
                    i += 1;
                } else {
                    problems.push("Flag '--repo' requires a value".to_string());
                }
            }
            arg if arg.starts_with('-') && arg != "-" => {
                problems.push(format!("Unknown flag '{}'", arg))
            }
            arg => positional.push(arg.to_string()),
        }
        i += 1;
    }
    (positional, dry_run, yes, repo)
}

/// Commands that accept no options at all. Silently ignoring `doctor --json`
/// made the flag look supported.
fn unexpected_arguments(command: &str, args: &[String]) -> Option<Vec<String>> {
    if args.is_empty() {
        return None;
    }
    Some(
        args.iter()
            .map(|arg| {
                if arg.starts_with('-') {
                    format!("Unknown flag '{}'", arg)
                } else {
                    format!("'{}' takes no arguments, but got '{}'", command, arg)
                }
            })
            .collect(),
    )
}

/// Every command takes exactly one target. Extra positionals used to be dropped
/// silently, so `install foo bar` acted on one package and said nothing about
/// the other.
fn report_extra_targets(command: &str, positional: &[String], problems: &mut Vec<String>) {
    for extra in positional.iter().skip(1) {
        problems.push(format!(
            "Unexpected argument '{}' for '{}'; expected a single target",
            extra, command
        ));
    }
}

/// A blank target used to run a full discovery (or reach the engine) for
/// nothing; it is a usage error like a missing one.
fn report_blank_target(command: &str, positional: &[String], problems: &mut Vec<String>) {
    if positional.first().is_some_and(|t| t.trim().is_empty()) {
        problems.push(format!("The target for '{}' must not be empty", command));
    }
}

/// Exit code for an `ExecutionResult`. The plan was accepted and ran, so the
/// transport succeeded even when the operation did not: reporting 0 for
/// `ok: false` made failed builds and smoke tests look green to scripts.
fn exit_code_for_execution(result: &serde_json::Value) -> i32 {
    if result.get("ok").and_then(|v| v.as_bool()).unwrap_or(false) {
        0
    } else {
        1
    }
}

/// Exit code for a run that stopped at the plan. A dry run printed with no
/// such note reads like a successful execution, which is how a user ends up
/// believing a package was installed.
fn dry_run_only() -> i32 {
    eprintln!("note: --dry-run — plan only; nothing was executed.");
    0
}

/// A missing or malformed argument is a usage problem (exit 2), not an
/// operational failure (exit 1).
fn usage_error(message: &str) -> Result<i32, String> {
    eprintln!("Error: {}", message);
    Ok(2)
}

/// Unknown flags used to be dropped silently, so a mistyped `--dryrun` became a
/// real, unreviewed run instead of an error. The offending command's own help is
/// printed on stderr, next to the complaint rather than the whole command list.
fn usage_errors(command: &str, problems: &[String]) -> Result<i32, String> {
    for problem in problems {
        eprintln!("Error: {}", problem);
    }
    eprint!("{}", command_help(command));
    Ok(2)
}

fn handle_search(client: &mut DirectRpcClient, args: &[String]) -> Result<i32, String> {
    if wants_help(args) {
        print!("{}", command_help("search"));
        return Ok(0);
    }
    let mut problems = Vec::new();
    let (positional, _dry_run, _yes, repo) = parse_flags(args, &mut problems);
    report_extra_targets("search", &positional, &mut problems);
    report_blank_target("search", &positional, &mut problems);
    if positional.is_empty() {
        problems.push("Missing target name for search".to_string());
    }
    if !problems.is_empty() {
        return usage_errors("search", &problems);
    }
    let target = &positional[0];

    let mut params = json!({ "target": target });
    if let Some(r) = repo {
        params["repo"] = json!(r);
    }

    let val = client.call("v1.search", params)?;
    println!("{}", serde_json::to_string_pretty(&val).unwrap());
    Ok(0)
}

fn handle_inspect(client: &mut DirectRpcClient, args: &[String]) -> Result<i32, String> {
    if wants_help(args) {
        print!("{}", command_help("inspect"));
        return Ok(0);
    }
    let mut problems = Vec::new();
    let (positional, _dry_run, _yes, _repo) = parse_flags(args, &mut problems);
    report_extra_targets("inspect", &positional, &mut problems);
    report_blank_target("inspect", &positional, &mut problems);
    if positional.is_empty() {
        problems.push("Missing package file for inspect".to_string());
    }
    if !problems.is_empty() {
        return usage_errors("inspect", &problems);
    }
    let target = &positional[0];

    let val = client.call("v1.inspect", json!({ "target": target }))?;
    println!("{}", serde_json::to_string_pretty(&val).unwrap());
    Ok(0)
}

fn handle_config(client: &mut DirectRpcClient, args: &[String]) -> Result<i32, String> {
    if wants_help(args) {
        print!("{}", command_help("config"));
        return Ok(0);
    }
    if args.is_empty() {
        return usage_error("Usage: archbridge config <get|set> <key> [value]");
    }
    let sub = args[0].as_str();
    match sub {
        "get" => {
            let mut problems = Vec::new();
            let (positional, dry_run, yes, _repo) = parse_flags(&args[1..], &mut problems);
            if dry_run || yes {
                problems.push(
                    "'config get' only reads: '--dry-run' and '--yes' do not apply".to_string(),
                );
            }
            report_extra_targets("config get", &positional, &mut problems);
            if !problems.is_empty() {
                return usage_errors("config", &problems);
            }
            let key = positional.first().map(String::as_str).unwrap_or("all");
            let val = client.call("v1.config.get", json!({ "key": key }))?;
            println!("{}", serde_json::to_string_pretty(&val).unwrap());
            Ok(0)
        }
        "set" => {
            let mut problems = Vec::new();
            let (positional, dry_run, yes, _repo) = parse_flags(&args[1..], &mut problems);
            // `config set <key> <value>` takes exactly two positionals; only a
            // third one is surplus (the value used to be reported as an extra).
            for extra in positional.iter().skip(2) {
                problems.push(format!(
                    "Unexpected argument '{}' for 'config set'; expected <key> <value>",
                    extra
                ));
            }
            if positional.len() < 2 {
                problems.push("Usage: archbridge config set <key> <value>".to_string());
            }
            if !problems.is_empty() {
                return usage_errors("config", &problems);
            }
            let key = &positional[0];
            let value = &positional[1];

            let prepare_params = json!({
                "action": "config.set",
                "key": key,
                "value": value
            });
            let plan_val = client.call("v1.prepare", prepare_params)?;
            println!("{}", serde_json::to_string_pretty(&plan_val).unwrap());

            if dry_run {
                return Ok(dry_run_only());
            }

            let plan: Plan = serde_json::from_value(plan_val).map_err(|e| e.to_string())?;
            if plan.blocked.is_some() {
                eprintln!("Plan is blocked and cannot be executed.");
                return Ok(3);
            }

            if !yes {
                eprint!("Confirm configuration change [y/N]? ");
                io::stderr().flush().ok();
                let mut input = String::new();
                io::stdin().read_line(&mut input).ok();
                if !input.trim().eq_ignore_ascii_case("y")
                    && !input.trim().eq_ignore_ascii_case("yes")
                {
                    eprintln!("Operation cancelled.");
                    return Ok(3);
                }
            }

            let exec_params = json!({
                "plan_id": plan.plan_id,
                "confirmed": true
            });
            let exec_res = client.call("v1.execute", exec_params)?;
            println!("{}", serde_json::to_string_pretty(&exec_res).unwrap());
            Ok(exit_code_for_execution(&exec_res))
        }
        _ => usage_error(&format!("Unknown config subcommand '{}'", sub)),
    }
}

fn handle_doctor(client: &mut DirectRpcClient, args: &[String]) -> Result<i32, String> {
    if wants_help(args) {
        print!("{}", command_help("doctor"));
        return Ok(0);
    }
    if let Some(problems) = unexpected_arguments("doctor", args) {
        return usage_errors("doctor", &problems);
    }
    let val = client.call("v1.doctor", json!({}))?;
    println!("{}", serde_json::to_string_pretty(&val).unwrap());
    if val.get("ready").and_then(|r| r.as_bool()).unwrap_or(false) {
        return Ok(0);
    }
    // Exiting 1 without naming the reason is the least useful answer a
    // diagnostic can give, so every check that holds readiness back is
    // listed — the host itself first.
    let checks: Vec<serde_json::Value> = val
        .get("checks")
        .and_then(|c| c.as_array())
        .cloned()
        .unwrap_or_default();
    let blocking: Vec<String> = checks
        .iter()
        .filter_map(|check| {
            let name = check.get("name").and_then(|n| n.as_str()).unwrap_or("?");
            let status = check.get("status").and_then(|s| s.as_str()).unwrap_or("");
            let message = check
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("")
                .to_string();
            let essential = archbridge::doctor::ESSENTIAL_CHECKS.contains(&name);
            ((essential && status != "pass") || status == "fail")
                .then(|| format!("{name} ({status}): {message}"))
        })
        .collect();
    eprintln!("Not ready: ArchBridge could not confirm this system for building or installing.");
    if blocking.is_empty() {
        eprintln!("  no check failed outright; see the report above for what did not pass.");
    } else {
        for reason in blocking {
            eprintln!("  blocking: {reason}");
        }
    }
    Ok(1)
}

/// The JSON-RPC daemon takes no options. `serve --help` used to swallow the
/// flag and block on stdin instead of answering the question.
fn handle_serve(args: &[String]) -> Result<i32, String> {
    if wants_help(args) {
        print!("{}", command_help("serve"));
        return Ok(0);
    }
    if let Some(problems) = unexpected_arguments("serve", args) {
        return usage_errors("serve", &problems);
    }
    let mut server = RpcServer::new();
    let stdin = BufReader::new(io::stdin());
    let stdout = io::stdout();
    server.run_loop(stdin, stdout)?;
    Ok(0)
}

#[rustfmt::skip]
fn handle_build(client: &mut DirectRpcClient, args: &[String]) -> Result<i32, String> {
    if wants_help(args) {
        print!("{}", command_help("build"));
        return Ok(0);
    }
    let mut target = None;
    let mut name = None;
    let mut version = None;
    let mut entry = None;
    let mut deps = Vec::new();
    let mut smoke_args = Vec::new();
    let mut dry_run = false;
    let mut yes = false;

    let mut i = 0;
    let mut problems: Vec<String> = Vec::new();
    while i < args.len() {
        match args[i].as_str() {
            "--dry-run" => dry_run = true,
            "--yes" | "-y" => yes = true,
            "--name" => { if i + 1 < args.len() { name = Some(args[i + 1].clone()); i += 1; } else { problems.push("Flag '--name' requires a value".to_string()); } }
            "--version" => { if i + 1 < args.len() { version = Some(args[i + 1].clone()); i += 1; } else { problems.push("Flag '--version' requires a value".to_string()); } }
            "--entry" => { if i + 1 < args.len() { entry = Some(args[i + 1].clone()); i += 1; } else { problems.push("Flag '--entry' requires a value".to_string()); } }
            "--dependency" => { if i + 1 < args.len() { deps.push(args[i + 1].clone()); i += 1; } else { problems.push("Flag '--dependency' requires a value".to_string()); } }
            "--smoke-arg" => { if i + 1 < args.len() { smoke_args.push(args[i + 1].clone()); i += 1; } else { problems.push("Flag '--smoke-arg' requires a value".to_string()); } }
            arg if arg.starts_with('-') && arg != "-" => problems.push(format!("Unknown flag '{}'", arg)),
            arg if target.is_none() => { target = Some(arg.to_string()); }
            arg => problems.push(format!("Unexpected argument '{}'", arg)),
        }
        i += 1;
    }

    let tgt = match target {
        Some(t) => t,
        None => {
            problems.push("Missing target for build".to_string());
            return usage_errors("build", &problems);
        }
    };
    let req_params = json!({
        "action": "build",
        "request": {
            "target": tgt,
            "options": {
                "name": name,
                "version": version,
                "entry": entry,
                "dependencies": deps,
                "smoke_args": smoke_args
            }
        }
    });

    let plan_val = client.call("v1.prepare", req_params)?;
    println!("{}", serde_json::to_string_pretty(&plan_val).unwrap());

    if dry_run { return Ok(dry_run_only()); }

    let plan: Plan = serde_json::from_value(plan_val.clone()).map_err(|e| e.to_string())?;
    if plan.blocked.is_some() {
        eprintln!("Plan is blocked and cannot be executed.");
        return Ok(3);
    }

    if !yes {
        eprint!("Confirm build execution [y/N]? ");
        io::stderr().flush().ok();
        let mut input = String::new();
        io::stdin().read_line(&mut input).ok();
        if !input.trim().eq_ignore_ascii_case("y") && !input.trim().eq_ignore_ascii_case("yes") {
            eprintln!("Operation cancelled.");
            return Ok(3);
        }
    }

    let exec_res = client.call("v1.execute", json!({ "plan_id": plan.plan_id, "confirmed": true }))?;
    println!("{}", serde_json::to_string_pretty(&exec_res).unwrap());
    Ok(exit_code_for_execution(&exec_res))
}

#[rustfmt::skip]
fn handle_install(client: &mut DirectRpcClient, args: &[String]) -> Result<i32, String> {
    if wants_help(args) {
        print!("{}", command_help("install"));
        return Ok(0);
    }
    let mut problems = Vec::new();
    let (positional, dry_run, yes, _repo) = parse_flags(args, &mut problems);
    report_extra_targets("install", &positional, &mut problems);
    report_blank_target("install", &positional, &mut problems);
    if positional.is_empty() {
        problems.push("Missing target for install".to_string());
    }
    if !problems.is_empty() {
        return usage_errors("install", &problems);
    }
    let target = &positional[0];

    let req_params = json!({
        "action": "install",
        "request": { "target": target }
    });

    let plan_val = client.call("v1.prepare", req_params)?;
    println!("{}", serde_json::to_string_pretty(&plan_val).unwrap());

    if dry_run { return Ok(dry_run_only()); }

    let plan: Plan = serde_json::from_value(plan_val.clone()).map_err(|e| e.to_string())?;
    if plan.blocked.is_some() {
        eprintln!("Plan is blocked and cannot be executed.");
        return Ok(3);
    }

    if !yes {
        eprint!("Confirm installation [y/N]? ");
        io::stderr().flush().ok();
        let mut input = String::new();
        io::stdin().read_line(&mut input).ok();
        if !input.trim().eq_ignore_ascii_case("y") && !input.trim().eq_ignore_ascii_case("yes") {
            eprintln!("Operation cancelled.");
            return Ok(3);
        }
    }

    let exec_res = client.call("v1.execute", json!({ "plan_id": plan.plan_id, "confirmed": true }))?;
    println!("{}", serde_json::to_string_pretty(&exec_res).unwrap());
    Ok(exit_code_for_execution(&exec_res))
}

#[rustfmt::skip]
fn handle_uninstall(client: &mut DirectRpcClient, args: &[String]) -> Result<i32, String> {
    if wants_help(args) {
        print!("{}", command_help("uninstall"));
        return Ok(0);
    }
    let mut problems = Vec::new();
    let (positional, dry_run, yes, _repo) = parse_flags(args, &mut problems);
    report_extra_targets("uninstall", &positional, &mut problems);
    report_blank_target("uninstall", &positional, &mut problems);
    if positional.is_empty() {
        problems.push("Missing installed package name for uninstall".to_string());
    }
    if !problems.is_empty() {
        return usage_errors("uninstall", &problems);
    }
    let target = &positional[0];
    let plan_val = client.call("v1.prepare", json!({
        "action": "uninstall",
        "request": { "target": target }
    }))?;
    println!("{}", serde_json::to_string_pretty(&plan_val).unwrap());
    if dry_run { return Ok(dry_run_only()); }

    let plan: Plan = serde_json::from_value(plan_val).map_err(|e| e.to_string())?;
    if plan.blocked.is_some() {
        eprintln!("Plan is blocked and cannot be executed.");
        return Ok(3);
    }
    if !yes {
        eprint!("Confirm package removal [y/N]? ");
        io::stderr().flush().ok();
        let mut input = String::new();
        io::stdin().read_line(&mut input).ok();
        if !input.trim().eq_ignore_ascii_case("y") && !input.trim().eq_ignore_ascii_case("yes") {
            eprintln!("Operation cancelled.");
            return Ok(3);
        }
    }
    let result = client.call("v1.execute", json!({ "plan_id": plan.plan_id, "confirmed": true }))?;
    println!("{}", serde_json::to_string_pretty(&result).unwrap());
    Ok(exit_code_for_execution(&result))
}

#[rustfmt::skip]
fn handle_test(client: &mut DirectRpcClient, args: &[String]) -> Result<i32, String> {
    if wants_help(args) {
        print!("{}", command_help("test"));
        return Ok(0);
    }
    let mut pkg_path = None;
    let mut entry = None;
    let mut smoke_args = Vec::new();
    let mut dry_run = false;
    let mut yes = false;

    let mut i = 0;
    let mut problems: Vec<String> = Vec::new();
    while i < args.len() {
        match args[i].as_str() {
            "--dry-run" => dry_run = true,
            "--yes" | "-y" => yes = true,
            "--entry" => { if i + 1 < args.len() { entry = Some(args[i + 1].clone()); i += 1; } else { problems.push("Flag '--entry' requires a value".to_string()); } }
            "--smoke-arg" => { if i + 1 < args.len() { smoke_args.push(args[i + 1].clone()); i += 1; } else { problems.push("Flag '--smoke-arg' requires a value".to_string()); } }
            arg if arg.starts_with('-') && arg != "-" => problems.push(format!("Unknown flag '{}'", arg)),
            arg if pkg_path.is_none() => { pkg_path = Some(arg.to_string()); }
            arg => problems.push(format!("Unexpected argument '{}'", arg)),
        }
        i += 1;
    }

    let pkg = match pkg_path {
        Some(p) => p,
        None => {
            problems.push("Missing package file for test".to_string());
            return usage_errors("test", &problems);
        }
    };

    let req_params = json!({
        "action": "test",
        "request": {
            "target": pkg,
            "options": {
                "entry": entry,
                "smoke_args": smoke_args
            }
        }
    });

    let plan_val = client.call("v1.prepare", req_params)?;
    println!("{}", serde_json::to_string_pretty(&plan_val).unwrap());

    if dry_run { return Ok(dry_run_only()); }

    let plan: Plan = serde_json::from_value(plan_val).map_err(|e| e.to_string())?;
    if plan.blocked.is_some() {
        eprintln!("Plan is blocked and cannot be executed.");
        return Ok(3);
    }

    if !yes {
        eprint!("Confirm test execution [y/N]? ");
        io::stderr().flush().ok();
        let mut input = String::new();
        io::stdin().read_line(&mut input).ok();
        if !input.trim().eq_ignore_ascii_case("y") && !input.trim().eq_ignore_ascii_case("yes") {
            eprintln!("Operation cancelled.");
            return Ok(3);
        }
    }

    let exec_res = client.call("v1.execute", json!({ "plan_id": plan.plan_id, "confirmed": true }))?;
    println!("{}", serde_json::to_string_pretty(&exec_res).unwrap());
    Ok(exit_code_for_execution(&exec_res))
}
