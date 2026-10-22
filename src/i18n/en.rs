//! English dictionary for the server and the terminal (spec 102). Same keys and parameters as
//! `pt_br` (checked in `i18n::tests`). No dashes or emoji (`tests/text_style.rs`).

pub const MESSAGES: &[(&str, &str)] = &[
    // Help and executable messages.
    (
        "cli.usage",
        "usage: nants [--no-open] [--lan] [--port N] [--config PATH] | nants serve [--port N] [--host H | --lan] [--config PATH] [--allow-remote] [--no-open] | nants doctor [--config PATH] | nants uninstall [--purge] | nants --version\n\
nants         starts Nants and opens the browser (--no-open turns this off); on 127.0.0.1, or on the local network if turned on in Settings > Network access\n\
nants serve   starts Nants without opening the browser\n\
nants doctor  checks the environment (Docker, Compose, Git, port, disk) without changing anything\n\
nants uninstall          removes the executable; keeps settings, data and projects (asks first)\n\
nants uninstall --purge  also removes the settings and data; never the projects or Docker\n\
--port N      fixed port; fails if it is busy (without --port, the default 3000 or the next free one)\n\
--lan         accepts access from the local network (0.0.0.0) for this run only, with the app preview ports right after the port (3001 to 3010); to keep it on, use Settings > Network access\n\
--allow-remote  allows a --host outside loopback (the server is exposed on that address)",
    ),
    ("cli.unknown_command", "unknown command: {command}"),
    ("cli.unknown_arg", "unknown argument: {arg}"),
    ("cli.needs_value", "{flag} needs a value"),
    ("cli.bad_port", "invalid port: {value}"),
    ("cli.lan_with_host", "--lan cannot be combined with --host (LAN mode uses 0.0.0.0)"),
    ("cli.error", "error: {message}"),
    ("cli.config_invalid_at", "invalid config (byte position {position})"),
    ("cli.config_invalid", "invalid config (malformed TOML)"),
    ("cli.config_serialize", "could not serialize the configuration"),
    ("cli.config_io", "I/O error: {kind}"),
    ("cli.config_missing_env", "missing environment variable: {name}"),
    ("cli.config_load_failed", "could not load {path}: {reason}"),
    ("cli.doctor_config_unreadable", "warning: could not read {path} ({reason}); using the default configuration"),
    ("cli.doctor_config_default", "warning: {reason}; using the default configuration"),
    ("cli.port_fallback", "port {port} is busy; using port {next}"),
    ("cli.ports_busy", "ports {port} to {last} are busy on {host}; free one of them or pick another with --port N"),
    ("cli.port_in_use", "port {port} is already in use on {host}; close the program using it or pick another with --port N"),
    ("cli.bind_failed", "could not open {host}:{port}: {error}"),
    ("cli.not_loopback", "host {host} is not loopback; use --allow-remote to allow it"),
    ("cli.exposed", "warning: server exposed on {host}; the Host check only accepts loopback names"),
    ("cli.local_addr_failed", "could not get the local address: {error}"),
    ("cli.view_ports", "  app preview ports: {start} to {end}"),
    ("cli.lan_warning", "warning: local network access is on; any device on the local network can control Nants (no authentication)"),
    (
        "cli.lan_adopted",
        "Local network access turned on (Windows is already set up). To turn it off: Settings > Network access.",
    ),
    ("cli.lan_save_failed", "warning: could not save the option in the settings ({error}); it applies only to this run"),
    (
        "cli.port_redirected",
        "port {port} is redirected by Windows (network access set up before); Nants is on port {next}. To use port {port} again, turn local network access on in Settings > Network access or undo the redirect there.",
    ),
    (
        "cli.port_redirected_explicit",
        "port {port} is redirected by Windows (network access set up before), so in local mode the browser would not open the page. Pick another one with --port N, turn local network access on in Settings > Network access or undo the redirect there.",
    ),
    ("cli.open_browser", "open in the browser: {url}"),
    ("cli.open_failed", "could not open the browser; open {url} yourself"),
    ("cli.server_error", "server error: {error}"),
    // nants uninstall (spec 103, decision 7).
    ("uninstall.will_remove", "nants uninstall will remove:"),
    ("uninstall.item.executable", "  the executable: {path}"),
    ("uninstall.item.config", "  the settings: {path}"),
    ("uninstall.item.data", "  the Nants data: {path}"),
    ("uninstall.keeps", "Not removed:"),
    (
        "uninstall.keep.config_data",
        "  the settings ({config}) and the data ({data}); to remove them too, use nants uninstall --purge",
    ),
    (
        "uninstall.keep.config_unreadable",
        "  {path}, because the settings file {config} could not be read and the projects folder may be inside it; fix the file and run again",
    ),
    ("uninstall.keep.inside_projects", "  {path}, because it holds the projects folder or is inside it"),
    ("uninstall.keep.projects", "  your projects in {path}"),
    (
        "uninstall.keep.docker",
        "  the Docker resources of the projects (label nants.project). To list them: {command}",
    ),
    ("uninstall.keep.other_exe", "  this executable ({path}) stays where it is: the installer only uses {installed}"),
    ("uninstall.nothing", "Nothing to remove: Nants is not installed at {path}."),
    ("uninstall.prompt", "Confirm? Type y and Enter to remove (just Enter cancels):"),
    (
        "uninstall.no_terminal",
        "No terminal to confirm: nothing was removed. Run {command} directly in a terminal and answer y.",
    ),
    ("uninstall.canceled", "Canceled: nothing was removed."),
    ("uninstall.removed", "removed: {path}"),
    ("uninstall.failed", "could not remove {path}: {error}"),
    ("uninstall.partial", "Some removals failed; see above."),
    ("uninstall.done", "Nants uninstalled. To install it again: {command}"),
    ("uninstall.no_home", "could not find the home folder (HOME); nothing was removed"),
    // nants doctor: report frame.
    ("doctor.title", "nants {version}: environment check"),
    ("doctor.status.ok", "OK"),
    ("doctor.status.fail", "FAIL"),
    ("doctor.status.info", "INFO"),
    ("doctor.status.warn", "WARN"),
    ("doctor.action", ". Action: {fix}"),
    ("doctor.summary.ok", "All set. Type nants to open Nants in the browser."),
    ("doctor.summary.fail", "Something required is missing: follow the action of the FAIL items and run nants doctor again."),
    // nants doctor: labels by the stable item id.
    ("doctor.label.git", "Git"),
    ("doctor.label.docker_cli", "Docker CLI"),
    ("doctor.label.docker_daemon", "Docker daemon"),
    ("doctor.label.docker_permission", "Docker permission"),
    ("doctor.label.docker_compose", "Docker Compose v2"),
    ("doctor.label.port", "Nants port"),
    ("doctor.label.disk", "Disk space"),
    ("doctor.label.wsl", "WSL"),
    ("doctor.label.windows_port_redirect", "Local network access"),
    ("doctor.label.playwright_images", "Playwright images"),
    ("doctor.label.version", "Nants version"),
    // nants doctor: suggested action by the stable item id.
    ("doctor.fix.git", "Install Git: sudo apt install git (Debian/Ubuntu/WSL). Then reload this screen."),
    (
        "doctor.fix.docker_cli",
        "Install Docker Engine (https://docs.docker.com/engine/install/) or, on Windows, Docker Desktop with WSL integration turned on (Settings > Resources > WSL integration). Nants does not install Docker.",
    ),
    (
        "doctor.fix.docker_daemon",
        "Start Docker: sudo systemctl start docker (Linux) or open Docker Desktop (Windows/WSL) and turn on the integration with your WSL distribution.",
    ),
    (
        "doctor.fix.docker_permission",
        "Add your user to the docker group: sudo usermod -aG docker $USER and start a new session (close and reopen the terminal; on WSL, run wsl --shutdown on Windows).",
    ),
    (
        "doctor.fix.docker_compose",
        "Install the Compose v2 plugin: sudo apt install docker-compose-plugin (or update Docker Desktop).",
    ),
    ("doctor.fix.port", "Close the program using these ports or pick another one with nants --port N."),
    ("doctor.fix.disk", "Free up disk space: Nants needs at least 5 GB free for Docker images and projects."),
    ("doctor.fix.version", "To update, run: {command}"),
    (
        "doctor.fix.windows_port_redirect",
        "Open Settings > Network access: turn local network access on or off and run the script shown there (set up or undo) in PowerShell as administrator.",
    ),
    // nants doctor: details.
    ("doctor.detail.git_missing", "git not found"),
    ("doctor.detail.docker_missing", "docker not found"),
    ("doctor.detail.depends_on_cli", "depends on the Docker CLI"),
    ("doctor.detail.daemon_unchecked_permission", "not checked: no permission on the socket"),
    ("doctor.detail.permission_denied", "permission denied on the socket"),
    ("doctor.detail.daemon_unreachable", "daemon unreachable"),
    ("doctor.detail.permission_unchecked", "not checked: daemon unreachable"),
    ("doctor.detail.compose_missing", "compose plugin missing"),
    ("doctor.detail.daemon_timeout", "did not answer in time"),
    ("doctor.detail.port_auto", "port chosen by the system at startup"),
    ("doctor.detail.port_free", "port {port} is free"),
    ("doctor.detail.port_fallback", "port {port} is busy (maybe Nants is already open); nants will use {next}"),
    ("doctor.detail.port_all_busy", "ports {port} to {last} are busy"),
    ("doctor.detail.disk_unmeasured", "could not measure free space"),
    ("doctor.detail.disk", "{list}"),
    ("doctor.detail.disk_free", "{size} free in {path}"),
    (
        "doctor.detail.wsl_detected",
        "detected: Docker may be Docker Desktop with WSL integration turned on or Docker Engine inside the distribution",
    ),
    ("doctor.detail.wsl_absent", "not detected (Linux)"),
    (
        "doctor.detail.env_wsl_nat",
        "WSL in NAT networking mode (the default); Docker may be Docker Desktop with WSL integration turned on or Docker Engine inside the distribution",
    ),
    (
        "doctor.detail.env_wsl_mirrored",
        "WSL in mirrored networking mode; Docker may be Docker Desktop with WSL integration turned on or Docker Engine inside the distribution",
    ),
    (
        "doctor.detail.env_wsl_unknown",
        "WSL (could not check the networking mode; treated as NAT); Docker may be Docker Desktop with WSL integration turned on or Docker Engine inside the distribution",
    ),
    (
        "doctor.detail.windows_redirect_on",
        "Windows is redirecting port {port} to WSL (network access set up before), but local network access is off: in local mode the browser does not open the page on that port and nants uses another one. Turn local network access on or undo the redirect",
    ),
    ("doctor.detail.windows_redirect_off", "local network access is off and Windows does not redirect port {port}"),
    ("doctor.detail.windows_redirect_unknown", "could not check with the Windows netsh.exe"),
    ("doctor.detail.firewall_unknown", "could not check the Windows firewall rule with PowerShell"),
    ("doctor.detail.lan_on_ok", "local network access is on and Windows is set up for port {port}"),
    (
        "doctor.detail.lan_on_mirrored_ok",
        "local network access is on and the Windows firewall rule Nants exists (mirrored mode, port {port})",
    ),
    (
        "doctor.detail.lan_on_no_redirect",
        "local network access is on, but Windows does not redirect port {port} to WSL: other devices cannot reach Nants. Run the script",
    ),
    (
        "doctor.detail.lan_on_other_target",
        "local network access is on, but Windows redirects port {port} to another address (the WSL IP changes when Windows restarts). Run the script again",
    ),
    (
        "doctor.detail.lan_on_no_firewall",
        "local network access is on, but the Windows firewall rule Nants is missing: other devices cannot reach port {port}. Run the script",
    ),
    (
        "doctor.detail.lan_will_adopt",
        "Windows already redirects port {port} to this WSL and has the firewall rule Nants: the next nants start turns local network access on",
    ),
    (
        "doctor.detail.lan_linux_on",
        "on: Nants listens on the local network (port {port}); if another device cannot open it, allow the port in the Linux firewall",
    ),
    (
        "doctor.detail.lan_linux_off",
        "off: Nants opens only on this computer; to turn it on, use Settings > Network access",
    ),
    (
        "doctor.detail.playwright_no_docker",
        "not checked: Docker unavailable. The first browser test of a project downloads about 2 GB",
    ),
    (
        "doctor.detail.playwright_list_failed",
        "could not list the images. The first browser test of a project downloads about 2 GB",
    ),
    ("doctor.detail.playwright_none", "none downloaded yet: the first browser test of a project downloads about 2 GB"),
    (
        "doctor.detail.playwright_some",
        "already downloaded: {tags}. Projects with another version download about 2 GB on the first browser test",
    ),
    ("doctor.detail.version_unknown", "{local} installed; could not check for a new version ({why})"),
    ("doctor.detail.version_available", "{local} installed; {latest} is available"),
    ("doctor.detail.version_latest", "{local}, the latest"),
    ("doctor.why.bad_manifest_version", "manifest with an invalid version"),
    ("doctor.why.no_url", "versions address not configured"),
    ("doctor.why.bad_url", "versions address is invalid"),
    ("doctor.why.no_client", "HTTP client unavailable"),
    ("doctor.why.no_connection", "no connection to the versions server"),
    ("doctor.why.http", "manifest unavailable, HTTP {status}"),
    ("doctor.why.incomplete", "incomplete manifest"),
    ("doctor.why.too_large", "manifest too large"),
    ("doctor.why.invalid", "invalid manifest"),
    ("doctor.why.no_version", "manifest without a version"),
    // Final report: fixed texts written by the server.
    ("report.env_clean", "_Test environment cleaned up (Compose `-test` containers, volumes and networks removed)._"),
    ("report.traceability.title", "## Traceability"),
    ("report.traceability.commit", "{id}, commit {hash}"),
    ("report.traceability.no_commit", "{id}, no integration commit"),
    ("report.traceability.no_task", "no task declared"),
    // Runner: tarefa de correção criada pela verificação final (spec 107), gravada no Txx.md.
    ("runner.final_fix_title", "Final verification fix"),
];
