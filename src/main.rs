use archbridge::engine::Plan;
use archbridge::rpc::RpcServer;
use serde_json::json;
use std::env;
use std::io::{self, BufReader, Write};

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        print_usage();
        std::process::exit(2);
    }

    let command = args[1].as_str();
    if command == "gui" || command == "--gui" {
        let script = std::path::Path::new("archbridge-gui.py");
        let script_path = if script.exists() {
            script.to_path_buf()
        } else if let Ok(exec_path) = std::env::current_exe() {
            exec_path
                .parent()
                .unwrap_or_else(|| std::path::Path::new("."))
                .join("archbridge-gui.py")
        } else {
            script.to_path_buf()
        };

        if script_path.exists() {
            let status = std::process::Command::new("python3")
                .arg(script_path)
                .status();
            std::process::exit(status.map_or(1, |s| s.code().unwrap_or(0)));
        } else {
            eprintln!("GUI script 'archbridge-gui.py' not found.");
            std::process::exit(1);
        }
    }

    if command == "serve" {
        let mut server = RpcServer::new();
        let stdin = BufReader::new(io::stdin());
        let stdout = io::stdout();
        if let Err(e) = server.run_loop(stdin, stdout) {
            eprintln!("RPC server error: {}", e);
            std::process::exit(1);
        }
        return;
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
        "doctor" => handle_doctor(&mut client),
        "-h" | "--help" | "help" => {
            print_usage();
            Ok(0)
        }
        _ => {
            eprintln!("Unknown command: '{}'", command);
            print_usage();
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

fn print_usage() {
    eprintln!(
        "ArchBridge — software discovery and packaging assistant for Arch Linux\n\n\
USAGE:\n  \
archbridge search <name> [--repo <url>]\n  \
archbridge info <name>\n  \
archbridge inspect <file.deb|file.rpm>\n  \
archbridge build <url|directory|PKGBUILD> [--name <name>] [--version <ver>] [--entry <bin>] [--dependency <pkg>...] [--smoke-arg <arg>...] [--dry-run] [--yes]\n  \
  archbridge install <name|file> [--dry-run] [--yes]\n  \
  archbridge uninstall <installed-package-name> [--dry-run] [--yes]\n  \
archbridge test <package.pkg.tar.zst> [--entry <bin>] [--smoke-arg <arg>...] [--dry-run] [--yes]\n  \
archbridge config <get|set> <key> [value] [--yes]\n  \
archbridge doctor\n  \
archbridge serve\n"
    );
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

fn parse_flags(args: &[String]) -> (Vec<String>, bool, bool, Option<String>) {
    let mut positional = Vec::new();
    let mut dry_run = false;
    let mut yes = false;
    let mut repo = None;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--dry-run" => dry_run = true,
            "--yes" | "-y" => yes = true,
            "--repo" => {
                if i + 1 < args.len() {
                    repo = Some(args[i + 1].clone());
                    i += 1;
                }
            }
            arg if !arg.starts_with("--") => positional.push(arg.to_string()),
            _ => {}
        }
        i += 1;
    }
    (positional, dry_run, yes, repo)
}

fn handle_search(client: &mut DirectRpcClient, args: &[String]) -> Result<i32, String> {
    let (positional, _dry_run, _yes, repo) = parse_flags(args);
    if positional.is_empty() {
        return Err("Missing target name for search".to_string());
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
    let (positional, _dry_run, _yes, _repo) = parse_flags(args);
    if positional.is_empty() {
        return Err("Missing package file for inspect".to_string());
    }
    let target = &positional[0];

    let val = client.call("v1.inspect", json!({ "target": target }))?;
    println!("{}", serde_json::to_string_pretty(&val).unwrap());
    Ok(0)
}

#[rustfmt::skip]
fn handle_config(client: &mut DirectRpcClient, args: &[String]) -> Result<i32, String> {
    if args.is_empty() {
        return Err("Usage: archbridge config <get|set> <key> [value]".to_string());
    }
    let sub = args[0].as_str();
    match sub {
        "get" => {
            let key = if args.len() > 1 { &args[1] } else { "all" };
            let val = client.call("v1.config.get", json!({ "key": key }))?;
            println!("{}", serde_json::to_string_pretty(&val).unwrap());
            Ok(0)
        }
        "set" => {
            let (positional, dry_run, yes, _repo) = parse_flags(&args[1..]);
            if positional.len() < 2 {
                return Err("Usage: archbridge config set <key> <value>".to_string());
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

            if dry_run { return Ok(0); }

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

            let plan: Plan = serde_json::from_value(plan_val).map_err(|e| e.to_string())?;
            let exec_params = json!({
                "plan_id": plan.plan_id,
                "confirmed": true
            });
            let exec_res = client.call("v1.execute", exec_params)?;
            println!("{}", serde_json::to_string_pretty(&exec_res).unwrap());
            Ok(0)
        }
        _ => Err(format!("Unknown config subcommand '{}'", sub)),
    }
}

fn handle_doctor(client: &mut DirectRpcClient) -> Result<i32, String> {
    let val = client.call("v1.doctor", json!({}))?;
    println!("{}", serde_json::to_string_pretty(&val).unwrap());
    if val.get("ready").and_then(|r| r.as_bool()).unwrap_or(false) {
        Ok(0)
    } else {
        Ok(1)
    }
}

#[rustfmt::skip]
fn handle_build(client: &mut DirectRpcClient, args: &[String]) -> Result<i32, String> {
    let mut target = None;
    let mut name = None;
    let mut version = None;
    let mut entry = None;
    let mut deps = Vec::new();
    let mut smoke_args = Vec::new();
    let mut dry_run = false;
    let mut yes = false;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--dry-run" => dry_run = true,
            "--yes" | "-y" => yes = true,
            "--name" => { if i + 1 < args.len() { name = Some(args[i + 1].clone()); i += 1; } }
            "--version" => { if i + 1 < args.len() { version = Some(args[i + 1].clone()); i += 1; } }
            "--entry" => { if i + 1 < args.len() { entry = Some(args[i + 1].clone()); i += 1; } }
            "--dependency" => { if i + 1 < args.len() { deps.push(args[i + 1].clone()); i += 1; } }
            "--smoke-arg" => { if i + 1 < args.len() { smoke_args.push(args[i + 1].clone()); i += 1; } }
            arg if !arg.starts_with("--") && target.is_none() => { target = Some(arg.to_string()); }
            _ => {}
        }
        i += 1;
    }

    let tgt = target.ok_or_else(|| "Missing target for build".to_string())?;

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

    if dry_run { return Ok(0); }

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
    Ok(0)
}

#[rustfmt::skip]
fn handle_install(client: &mut DirectRpcClient, args: &[String]) -> Result<i32, String> {
    let (positional, dry_run, yes, _repo) = parse_flags(args);
    if positional.is_empty() {
        return Err("Missing target for install".to_string());
    }
    let target = &positional[0];

    let req_params = json!({
        "action": "install",
        "request": { "target": target }
    });

    let plan_val = client.call("v1.prepare", req_params)?;
    println!("{}", serde_json::to_string_pretty(&plan_val).unwrap());

    if dry_run { return Ok(0); }

    let plan: Plan = serde_json::from_value(plan_val.clone()).map_err(|e| e.to_string())?;
    if plan.blocked.is_some() {
        eprintln!("Plan is blocked.");
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
    Ok(0)
}

#[rustfmt::skip]
fn handle_uninstall(client: &mut DirectRpcClient, args: &[String]) -> Result<i32, String> {
    let (positional, dry_run, yes, _repo) = parse_flags(args);
    if positional.is_empty() {
        return Err("Missing installed package name for uninstall".to_string());
    }
    let target = &positional[0];
    let plan_val = client.call("v1.prepare", json!({
        "action": "uninstall",
        "request": { "target": target }
    }))?;
    println!("{}", serde_json::to_string_pretty(&plan_val).unwrap());
    if dry_run { return Ok(0); }

    let plan: Plan = serde_json::from_value(plan_val).map_err(|e| e.to_string())?;
    if plan.blocked.is_some() { return Ok(3); }
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
    Ok(0)
}

#[rustfmt::skip]
fn handle_test(client: &mut DirectRpcClient, args: &[String]) -> Result<i32, String> {
    let mut pkg_path = None;
    let mut entry = None;
    let mut smoke_args = Vec::new();
    let mut dry_run = false;
    let mut yes = false;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--dry-run" => dry_run = true,
            "--yes" | "-y" => yes = true,
            "--entry" => { if i + 1 < args.len() { entry = Some(args[i + 1].clone()); i += 1; } }
            "--smoke-arg" => { if i + 1 < args.len() { smoke_args.push(args[i + 1].clone()); i += 1; } }
            arg if !arg.starts_with("--") && pkg_path.is_none() => { pkg_path = Some(arg.to_string()); }
            _ => {}
        }
        i += 1;
    }

    let pkg = pkg_path.ok_or_else(|| "Missing package file for test".to_string())?;

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

    if dry_run { return Ok(0); }

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

    let plan: Plan = serde_json::from_value(plan_val).map_err(|e| e.to_string())?;
    let exec_res = client.call("v1.execute", json!({ "plan_id": plan.plan_id, "confirmed": true }))?;
    println!("{}", serde_json::to_string_pretty(&exec_res).unwrap());
    Ok(0)
}
