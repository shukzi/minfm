use std::{
    collections::BTreeMap,
    env, fmt, fs,
    io::{Read, Write},
    os::unix::{fs::OpenOptionsExt, process::CommandExt},
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    sync::{
        atomic::{compiler_fence, AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

use percent_encoding::percent_decode_str;
use serde::Deserialize;

const COMMAND_OUTPUT_LIMIT: u64 = 1024 * 1024;
const DISCOVERY_LIMIT: usize = 256;
const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(8);
const OPERATION_TIMEOUT: Duration = Duration::from_secs(30);
static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Default)]
pub struct NetworkSecret(Vec<u8>);

impl NetworkSecret {
    pub fn push(&mut self, character: char) {
        let mut encoded = [0; 4];
        self.0
            .extend_from_slice(character.encode_utf8(&mut encoded).as_bytes());
        encoded.fill(0);
    }

    pub fn pop(&mut self) {
        if let Ok(text) = std::str::from_utf8(&self.0) {
            if let Some((index, _)) = text.char_indices().next_back() {
                self.0.truncate(index);
            }
        }
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn character_count(&self) -> usize {
        std::str::from_utf8(&self.0)
            .map(|text| text.chars().count())
            .unwrap_or_default()
    }

    fn from_bytes(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }

    fn expose(&self) -> &[u8] {
        &self.0
    }
}

impl fmt::Debug for NetworkSecret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("NetworkSecret([REDACTED])")
    }
}

impl Drop for NetworkSecret {
    fn drop(&mut self) {
        wipe(&mut self.0);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShareAddress {
    pub uri: String,
    pub server: String,
    pub share: String,
}

impl ShareAddress {
    pub fn parse(input: &str) -> Result<Self, String> {
        let input = input.trim();
        if input.is_empty() {
            return Err("Enter a Samba share such as smb://server/share".into());
        }
        if input.chars().any(char::is_control) || input.chars().any(char::is_whitespace) {
            return Err("The share address cannot contain spaces or control characters".into());
        }
        let with_scheme = if input
            .get(..6)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("smb://"))
        {
            format!("smb://{}", &input[6..])
        } else if input.contains("://") {
            return Err("Only smb:// addresses are supported".into());
        } else {
            format!("smb://{input}")
        };
        if with_scheme.contains('?') || with_scheme.contains('#') {
            return Err("Query strings and fragments are not allowed in share addresses".into());
        }
        let remainder = with_scheme[6..].trim_end_matches('/');
        let mut parts = remainder.split('/').filter(|part| !part.is_empty());
        let server = parts.next().unwrap_or_default();
        let share = parts.next().unwrap_or_default();
        if server.is_empty() || share.is_empty() {
            return Err("Enter both a server and share: smb://server/share".into());
        }
        if parts.next().is_some() {
            return Err("Enter the share itself, without a path inside it".into());
        }
        if server.contains('@') || server.contains(':') {
            return Err("Do not place credentials or a port in the share address".into());
        }
        let server_display = decode_component(server)?;
        let share_display = decode_component(share)?;
        Ok(Self {
            uri: format!("smb://{server}/{share}"),
            server: server_display,
            share: share_display,
        })
    }
}

fn decode_component(component: &str) -> Result<String, String> {
    percent_decode_str(component)
        .decode_utf8()
        .map(|decoded| decoded.into_owned())
        .map_err(|_| "The share address contains invalid encoded text".into())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetworkShare {
    pub address: ShareAddress,
    pub mount_path: Option<PathBuf>,
    pub username: Option<String>,
    pub domain: Option<String>,
    pub saved: bool,
    pub discovered: bool,
}

impl NetworkShare {
    pub fn state(&self) -> &'static str {
        if self.mount_path.is_some() {
            "connected"
        } else if self.saved {
            "remembered"
        } else {
            "available"
        }
    }

    pub fn account(&self) -> String {
        match (&self.domain, &self.username) {
            (Some(domain), Some(username)) if !domain.is_empty() => {
                format!("{domain}\\{username}")
            }
            (_, Some(username)) => username.clone(),
            _ => "—".into(),
        }
    }
}

#[derive(Debug, Clone)]
pub enum NetworkAuth {
    Anonymous,
    Password {
        username: String,
        domain: String,
        password: NetworkSecret,
        remember: bool,
    },
    Saved {
        username: String,
        domain: String,
    },
}

#[derive(Debug, Clone)]
pub struct ConnectRequest {
    pub address: ShareAddress,
    pub auth: NetworkAuth,
}

#[derive(Debug, Clone)]
pub enum NetworkAction {
    Connect(ConnectRequest),
    Disconnect(NetworkShare),
    Forget(NetworkShare),
}

#[derive(Debug)]
pub enum NetworkOutcome {
    Connected {
        address: ShareAddress,
        mount_path: Option<PathBuf>,
        remembered: bool,
    },
    Disconnected(String),
    Forgotten(String),
    CredentialsRequired {
        address: ShareAddress,
        username: String,
        domain: String,
        reason: String,
    },
}

#[derive(Debug, Clone)]
pub struct NetworkEnvironment {
    pub gio: PathBuf,
    pub secret_tool: Option<PathBuf>,
    pub runtime_dir: PathBuf,
    pub shares_file: PathBuf,
}

impl NetworkEnvironment {
    pub fn detect(config_path: &Path) -> Self {
        let runtime_dir = env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(format!("/run/user/{}", nix::unistd::Uid::current())));
        Self {
            gio: find_command("gio").unwrap_or_else(|| PathBuf::from("gio")),
            secret_tool: find_command("secret-tool"),
            runtime_dir,
            shares_file: config_path
                .parent()
                .unwrap_or_else(|| Path::new("."))
                .join("network-shares.toml"),
        }
    }

    pub fn samba_tools_available(&self) -> bool {
        self.gio.is_file() || find_command_path(&self.gio)
    }

    fn gvfs_root(&self) -> PathBuf {
        self.runtime_dir.join("gvfs")
    }
}

pub fn secret_service_available(environment: &NetworkEnvironment) -> bool {
    let Some(tool) = environment.secret_tool.as_ref() else {
        return false;
    };
    let mut command = Command::new(tool);
    command.args(["lookup", "application", "minfm-availability-probe"]);
    let Ok(mut output) = run_command(command, Duration::from_secs(2)) else {
        return false;
    };
    let available = output.status.success() || output.stderr.is_empty();
    wipe(&mut output.stdout);
    available
}

pub fn discover(environment: &NetworkEnvironment) -> Result<Vec<NetworkShare>, String> {
    if !environment.samba_tools_available() {
        return Err("Samba support is unavailable because gio is missing".into());
    }
    let saved = load_saved(&environment.shares_file)?;
    let mounted = discover_mounted(&environment.gvfs_root());
    let discovered = discover_remote(environment).unwrap_or_default();
    Ok(merge_shares(saved, mounted, discovered))
}

pub fn discover_local(environment: &NetworkEnvironment) -> Result<Vec<NetworkShare>, String> {
    if !environment.samba_tools_available() {
        return Err("Samba support is unavailable because gio is missing".into());
    }
    let saved = load_saved(&environment.shares_file)?;
    let mounted = discover_mounted(&environment.gvfs_root());
    Ok(merge_shares(saved, mounted, Vec::new()))
}

fn merge_shares(
    saved: Vec<SavedShare>,
    mounted: Vec<(ShareAddress, PathBuf)>,
    discovered: Vec<ShareAddress>,
) -> Vec<NetworkShare> {
    let mut shares = BTreeMap::<String, NetworkShare>::new();
    for saved in saved {
        let address = match ShareAddress::parse(&saved.uri) {
            Ok(address) => address,
            Err(_) => continue,
        };
        shares.insert(
            address.uri.clone(),
            NetworkShare {
                address,
                mount_path: None,
                username: Some(saved.username),
                domain: Some(saved.domain),
                saved: true,
                discovered: false,
            },
        );
    }
    for address in discovered {
        let entry = shares
            .entry(address.uri.clone())
            .or_insert_with(|| NetworkShare {
                address,
                mount_path: None,
                username: None,
                domain: None,
                saved: false,
                discovered: true,
            });
        entry.discovered = true;
    }
    for (address, mount_path) in mounted {
        let entry = shares
            .entry(address.uri.clone())
            .or_insert_with(|| NetworkShare {
                address,
                mount_path: None,
                username: None,
                domain: None,
                saved: false,
                discovered: false,
            });
        entry.mount_path = Some(mount_path);
    }
    shares.into_values().collect()
}

pub fn perform(
    action: NetworkAction,
    environment: &NetworkEnvironment,
) -> Result<NetworkOutcome, String> {
    match action {
        NetworkAction::Connect(request) => connect(request, environment),
        NetworkAction::Disconnect(share) => disconnect(&share, environment),
        NetworkAction::Forget(share) => forget(&share, environment),
    }
}

fn connect(
    request: ConnectRequest,
    environment: &NetworkEnvironment,
) -> Result<NetworkOutcome, String> {
    let looked_up = match &request.auth {
        NetworkAuth::Saved { username, domain } => {
            match lookup_secret(environment, &request.address, username, domain) {
                Ok(secret) => Some(secret),
                Err(error) => {
                    return Ok(NetworkOutcome::CredentialsRequired {
                        address: request.address,
                        username: username.clone(),
                        domain: domain.clone(),
                        reason: format!("Saved credentials could not be read: {error}"),
                    })
                }
            }
        }
        _ => None,
    };
    let (username, domain, password, anonymous, remember): (
        String,
        String,
        Option<&NetworkSecret>,
        bool,
        bool,
    ) = match &request.auth {
        NetworkAuth::Anonymous => (String::new(), String::new(), None, true, false),
        NetworkAuth::Password {
            username,
            domain,
            password,
            remember,
        } => (
            username.clone(),
            domain.clone(),
            Some(password),
            false,
            *remember,
        ),
        NetworkAuth::Saved { username, domain } => (
            username.clone(),
            domain.clone(),
            looked_up.as_ref(),
            false,
            false,
        ),
    };

    let mount_result = run_gio_mount(
        environment,
        &request.address,
        &username,
        &domain,
        password,
        anonymous,
    );
    if let Err(error) = mount_result {
        if matches!(request.auth, NetworkAuth::Saved { .. }) {
            return Ok(NetworkOutcome::CredentialsRequired {
                address: request.address,
                username,
                domain,
                reason: error,
            });
        }
        return Err(error);
    }

    let mut remembered = false;
    if remember {
        let password = password.ok_or_else(|| "No password was provided".to_string())?;
        store_secret(environment, &request.address, &username, &domain, password).map_err(
            |error| {
                format!("The share connected, but its password could not be remembered: {error}")
            },
        )?;
        let mut saved = load_saved(&environment.shares_file)?;
        saved.retain(|entry| entry.uri != request.address.uri);
        saved.push(SavedShare {
            uri: request.address.uri.clone(),
            username: username.clone(),
            domain: domain.clone(),
        });
        if let Err(error) = save_saved(&environment.shares_file, &saved) {
            let _ = clear_secret(environment, &request.address, &username, &domain);
            return Err(format!(
                "The connection succeeded, but remembering it failed: {error}"
            ));
        }
        remembered = true;
    }

    let mount_path = wait_for_mount_path(&environment.gvfs_root(), &request.address);
    drop(looked_up);
    Ok(NetworkOutcome::Connected {
        address: request.address,
        mount_path,
        remembered,
    })
}

fn disconnect(
    share: &NetworkShare,
    environment: &NetworkEnvironment,
) -> Result<NetworkOutcome, String> {
    let mut command = Command::new(&environment.gio);
    command
        .args(["mount", "--unmount", &share.address.uri])
        .env("LC_ALL", "C");
    let output = run_command(command, OPERATION_TIMEOUT)?;
    if !output.status.success() {
        return Err(command_failure("Disconnect failed", &output));
    }
    Ok(NetworkOutcome::Disconnected(format!(
        "Disconnected {}",
        share.address.uri
    )))
}

fn forget(
    share: &NetworkShare,
    environment: &NetworkEnvironment,
) -> Result<NetworkOutcome, String> {
    let username = share.username.clone().unwrap_or_default();
    let domain = share.domain.clone().unwrap_or_default();
    clear_secret(environment, &share.address, &username, &domain)?;
    let mut saved = load_saved(&environment.shares_file)?;
    saved.retain(|entry| entry.uri != share.address.uri);
    save_saved(&environment.shares_file, &saved)?;
    Ok(NetworkOutcome::Forgotten(format!(
        "Forgot {}",
        share.address.uri
    )))
}

fn run_gio_mount(
    environment: &NetworkEnvironment,
    address: &ShareAddress,
    username: &str,
    domain: &str,
    password: Option<&NetworkSecret>,
    anonymous: bool,
) -> Result<(), String> {
    let mut command = Command::new(&environment.gio);
    command.arg("mount");
    if anonymous {
        command.arg("--anonymous");
    }
    command
        .arg(&address.uri)
        .env("LC_ALL", "C")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    isolate_process_group(&mut command);
    let mut child = command
        .spawn()
        .map_err(|error| format!("Could not start gio: {error}"))?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| "Could not open the authentication channel".to_string())?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "Could not read gio output".to_string())?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "Could not read gio errors".to_string())?;
    let (sender, receiver) = mpsc::channel();
    spawn_stream_reader(stdout, StreamKind::Stdout, sender.clone());
    spawn_stream_reader(stderr, StreamKind::Stderr, sender);

    let started = Instant::now();
    let mut status = None;
    let mut stdout_data = Vec::new();
    let mut stderr_data = Vec::new();
    let mut prompt = String::new();
    let mut user_answers = 0usize;
    let mut domain_answers = 0usize;
    let mut password_answers = 0usize;
    while status.is_none() {
        match receiver.recv_timeout(Duration::from_millis(25)) {
            Ok(StreamMessage::Data(kind, data)) => {
                let target = match kind {
                    StreamKind::Stdout => &mut stdout_data,
                    StreamKind::Stderr => &mut stderr_data,
                };
                append_limited(target, &data);
                if kind == StreamKind::Stdout {
                    prompt.push_str(&String::from_utf8_lossy(&data));
                    if prompt_ready(&prompt, "User") {
                        user_answers += 1;
                        if user_answers > 1 {
                            terminate(&mut child);
                            return Err(
                                "Authentication was requested again; check the username".into()
                            );
                        }
                        write_answer(&mut stdin, username.as_bytes())?;
                        prompt.clear();
                    } else if prompt_ready(&prompt, "Domain") {
                        domain_answers += 1;
                        if domain_answers > 1 {
                            terminate(&mut child);
                            return Err(
                                "Authentication was requested again; check the domain".into()
                            );
                        }
                        write_answer(&mut stdin, domain.as_bytes())?;
                        prompt.clear();
                    } else if prompt_ready(&prompt, "Password") {
                        password_answers += 1;
                        if password_answers > 1 {
                            terminate(&mut child);
                            return Err("The server did not accept the credentials".into());
                        }
                        let Some(password) = password else {
                            terminate(&mut child);
                            return Err("The server requires a password".into());
                        };
                        write_answer(&mut stdin, password.expose())?;
                        prompt.clear();
                    }
                    if prompt.len() > 4096 {
                        let split = prompt.len().saturating_sub(1024);
                        prompt.drain(..split);
                    }
                }
            }
            Ok(StreamMessage::Finished) | Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {}
        }
        status = child
            .try_wait()
            .map_err(|error| format!("Could not monitor gio: {error}"))?;
        if started.elapsed() >= OPERATION_TIMEOUT {
            terminate(&mut child);
            return Err("Connection timed out after 30 seconds".into());
        }
    }
    drop(stdin);
    drain_streams(&receiver, &mut stdout_data, &mut stderr_data);
    if status.is_some_and(|status| status.success()) {
        Ok(())
    } else {
        Err(command_failure_parts(
            "Connection failed",
            &stdout_data,
            &stderr_data,
        ))
    }
}

fn prompt_ready(output: &str, label: &str) -> bool {
    let line = output.rsplit('\n').next().unwrap_or_default().trim_start();
    let Some(suffix) = line.strip_prefix(label) else {
        return false;
    };
    let suffix = suffix.trim();
    suffix == ":" || (suffix.starts_with('[') && suffix.ends_with("]:"))
}

fn write_answer(stdin: &mut impl Write, answer: &[u8]) -> Result<(), String> {
    stdin
        .write_all(answer)
        .and_then(|()| stdin.write_all(b"\n"))
        .and_then(|()| stdin.flush())
        .map_err(|error| format!("Could not send authentication response: {error}"))
}

fn discover_remote(environment: &NetworkEnvironment) -> Result<Vec<ShareAddress>, String> {
    let started = Instant::now();
    let roots = list_uris(environment, "smb://", DISCOVERY_TIMEOUT)?;
    let mut shares = BTreeMap::new();
    let mut containers = Vec::new();
    for uri in roots {
        match address_depth(&uri) {
            0 | 1 => containers.push(uri),
            _ => {
                if let Ok(address) = ShareAddress::parse(&uri) {
                    shares.insert(address.uri.clone(), address);
                }
            }
        }
    }
    for container in containers.into_iter().take(32) {
        let remaining = DISCOVERY_TIMEOUT.saturating_sub(started.elapsed());
        if remaining.is_zero() || shares.len() >= DISCOVERY_LIMIT {
            break;
        }
        for uri in list_uris(
            environment,
            &container,
            remaining.min(Duration::from_secs(2)),
        )
        .unwrap_or_default()
        {
            if let Ok(address) = ShareAddress::parse(&uri) {
                shares.insert(address.uri.clone(), address);
                if shares.len() >= DISCOVERY_LIMIT {
                    break;
                }
            }
        }
    }
    Ok(shares.into_values().collect())
}

fn list_uris(
    environment: &NetworkEnvironment,
    location: &str,
    timeout: Duration,
) -> Result<Vec<String>, String> {
    let mut command = Command::new(&environment.gio);
    command
        .args(["list", "--print-uris", location])
        .env("LC_ALL", "C");
    let output = run_command(command, timeout)?;
    if !output.status.success() {
        return Err(command_failure("Share discovery failed", &output));
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|line| line.starts_with("smb://"))
        .take(DISCOVERY_LIMIT)
        .map(str::to_owned)
        .collect())
}

fn address_depth(uri: &str) -> usize {
    uri.strip_prefix("smb://")
        .unwrap_or_default()
        .split('/')
        .filter(|part| !part.is_empty())
        .count()
}

fn discover_mounted(root: &Path) -> Vec<(ShareAddress, PathBuf)> {
    let Ok(entries) = fs::read_dir(root) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let file_name = entry.file_name();
            let text = file_name.to_string_lossy();
            let attributes = parse_gvfs_name(&text)?;
            let server = attributes.get("server")?;
            let share = attributes.get("share")?;
            let address = ShareAddress::parse(&format!("smb://{server}/{share}")).ok()?;
            Some((address, entry.path()))
        })
        .collect()
}

fn wait_for_mount_path(root: &Path, address: &ShareAddress) -> Option<PathBuf> {
    const SETTLE_TIMEOUT: Duration = Duration::from_secs(1);
    const RETRY_INTERVAL: Duration = Duration::from_millis(25);

    let deadline = Instant::now() + SETTLE_TIMEOUT;
    loop {
        if let Some(path) = find_mount_path(root, address) {
            return Some(path);
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return None;
        }
        thread::sleep(remaining.min(RETRY_INTERVAL));
    }
}

fn find_mount_path(root: &Path, address: &ShareAddress) -> Option<PathBuf> {
    discover_mounted(root)
        .into_iter()
        .find_map(|(mounted, path)| same_share(&mounted, address).then_some(path))
}

fn same_share(left: &ShareAddress, right: &ShareAddress) -> bool {
    left.server.eq_ignore_ascii_case(&right.server) && left.share.eq_ignore_ascii_case(&right.share)
}

fn parse_gvfs_name(name: &str) -> Option<BTreeMap<String, String>> {
    let values = name.strip_prefix("smb-share:")?;
    let mut result = BTreeMap::new();
    for pair in split_escaped(values, ',') {
        let (key, value) = split_once_escaped(&pair, '=')?;
        result.insert(unescape(&key), unescape(&value));
    }
    Some(result)
}

fn split_escaped(input: &str, delimiter: char) -> Vec<String> {
    let mut parts = vec![String::new()];
    let mut escaped = false;
    for character in input.chars() {
        if escaped {
            parts.last_mut().expect("one part").push('\\');
            parts.last_mut().expect("one part").push(character);
            escaped = false;
        } else if character == '\\' {
            escaped = true;
        } else if character == delimiter {
            parts.push(String::new());
        } else {
            parts.last_mut().expect("one part").push(character);
        }
    }
    if escaped {
        parts.last_mut().expect("one part").push('\\');
    }
    parts
}

fn split_once_escaped(input: &str, delimiter: char) -> Option<(String, String)> {
    let mut escaped = false;
    for (index, character) in input.char_indices() {
        if escaped {
            escaped = false;
        } else if character == '\\' {
            escaped = true;
        } else if character == delimiter {
            return Some((
                input[..index].into(),
                input[index + character.len_utf8()..].into(),
            ));
        }
    }
    None
}

fn unescape(input: &str) -> String {
    let mut result = String::new();
    let mut escaped = false;
    for character in input.chars() {
        if escaped {
            result.push(character);
            escaped = false;
        } else if character == '\\' {
            escaped = true;
        } else {
            result.push(character);
        }
    }
    if escaped {
        result.push('\\');
    }
    result
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct SavedShare {
    uri: String,
    username: String,
    #[serde(default)]
    domain: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct SavedShares {
    share: Vec<SavedShare>,
}

fn load_saved(path: &Path) -> Result<Vec<SavedShare>, String> {
    match fs::read_to_string(path) {
        Ok(text) => toml::from_str::<SavedShares>(&text)
            .map(|saved| saved.share)
            .map_err(|error| format!("Invalid saved-share file {}: {error}", path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(format!("Could not read {}: {error}", path.display())),
    }
}

fn save_saved(path: &Path, shares: &[SavedShare]) -> Result<(), String> {
    let Some(parent) = path.parent() else {
        return Err("The saved-share path has no parent directory".into());
    };
    fs::create_dir_all(parent)
        .map_err(|error| format!("Could not create {}: {error}", parent.display()))?;
    let mut text = String::new();
    for share in shares {
        text.push_str("[[share]]\nuri = \"");
        text.push_str(&toml_escape(&share.uri));
        text.push_str("\"\nusername = \"");
        text.push_str(&toml_escape(&share.username));
        text.push_str("\"\ndomain = \"");
        text.push_str(&toml_escape(&share.domain));
        text.push_str("\"\n\n");
    }
    let counter = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let temporary = parent.join(format!(
        ".network-shares.toml.tmp-{}-{counter}",
        std::process::id()
    ));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temporary)
        .map_err(|error| format!("Could not create {}: {error}", temporary.display()))?;
    let result = file
        .write_all(text.as_bytes())
        .and_then(|()| file.sync_all())
        .and_then(|()| fs::rename(&temporary, path));
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result.map_err(|error| format!("Could not save {}: {error}", path.display()))
}

fn toml_escape(value: &str) -> String {
    value
        .chars()
        .flat_map(|character| match character {
            '\\' => "\\\\".chars().collect::<Vec<_>>(),
            '"' => "\\\"".chars().collect(),
            '\n' => "\\n".chars().collect(),
            '\r' => "\\r".chars().collect(),
            '\t' => "\\t".chars().collect(),
            character => vec![character],
        })
        .collect()
}

fn store_secret(
    environment: &NetworkEnvironment,
    address: &ShareAddress,
    username: &str,
    domain: &str,
    password: &NetworkSecret,
) -> Result<(), String> {
    let tool = environment
        .secret_tool
        .as_ref()
        .ok_or_else(|| "Secret Service tools are not installed".to_string())?;
    let mut command = Command::new(tool);
    command
        .arg("store")
        .arg("--label=minfm Samba share")
        .arg("--")
        .args(secret_attributes(address, username, domain))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    isolate_process_group(&mut command);
    let mut child = command
        .spawn()
        .map_err(|error| format!("Could not start secret-tool: {error}"))?;
    child
        .stdin
        .take()
        .ok_or_else(|| "Could not open Secret Service input".to_string())?
        .write_all(password.expose())
        .map_err(|error| format!("Could not send the password to Secret Service: {error}"))?;
    let output = wait_for_child(child, OPERATION_TIMEOUT)?;
    if output.status.success() {
        Ok(())
    } else {
        Err(command_failure(
            "Secret Service rejected the password",
            &output,
        ))
    }
}

fn lookup_secret(
    environment: &NetworkEnvironment,
    address: &ShareAddress,
    username: &str,
    domain: &str,
) -> Result<NetworkSecret, String> {
    let tool = environment
        .secret_tool
        .as_ref()
        .ok_or_else(|| "Secret Service tools are not installed".to_string())?;
    let mut command = Command::new(tool);
    command
        .arg("lookup")
        .arg("--")
        .args(secret_attributes(address, username, domain));
    let mut output = run_command(command, OPERATION_TIMEOUT)?;
    if !output.status.success() || output.stdout.is_empty() {
        wipe(&mut output.stdout);
        return Err(command_failure("No saved password was available", &output));
    }
    while output
        .stdout
        .last()
        .is_some_and(|byte| matches!(byte, b'\n' | b'\r'))
    {
        output.stdout.pop();
    }
    Ok(NetworkSecret::from_bytes(output.stdout))
}

fn clear_secret(
    environment: &NetworkEnvironment,
    address: &ShareAddress,
    username: &str,
    domain: &str,
) -> Result<(), String> {
    let tool = environment
        .secret_tool
        .as_ref()
        .ok_or_else(|| "Secret Service tools are not installed".to_string())?;
    let mut command = Command::new(tool);
    command
        .arg("clear")
        .arg("--")
        .args(secret_attributes(address, username, domain));
    let output = run_command(command, OPERATION_TIMEOUT)?;
    if output.status.success() {
        Ok(())
    } else {
        Err(command_failure(
            "Could not remove the saved password",
            &output,
        ))
    }
}

fn secret_attributes<'a>(
    address: &'a ShareAddress,
    username: &'a str,
    domain: &'a str,
) -> [&'a str; 12] {
    [
        "application",
        "minfm",
        "protocol",
        "smb",
        "server",
        &address.server,
        "share",
        &address.share,
        "username",
        username,
        "domain",
        domain,
    ]
}

struct CommandOutput {
    status: ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

fn run_command(mut command: Command, timeout: Duration) -> Result<CommandOutput, String> {
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    isolate_process_group(&mut command);
    let child = command
        .spawn()
        .map_err(|error| format!("Could not start command: {error}"))?;
    wait_for_child(child, timeout)
}

fn wait_for_child(mut child: Child, timeout: Duration) -> Result<CommandOutput, String> {
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "Could not capture command output".to_string())?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "Could not capture command errors".to_string())?;
    let stdout_reader = thread::spawn(move || read_limited(stdout));
    let stderr_reader = thread::spawn(move || read_limited(stderr));
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child
            .try_wait()
            .map_err(|error| format!("Could not monitor command: {error}"))?
        {
            break status;
        }
        if started.elapsed() >= timeout {
            terminate(&mut child);
            let _ = stdout_reader.join();
            let _ = stderr_reader.join();
            return Err(format!(
                "Command timed out after {} seconds",
                timeout.as_secs()
            ));
        }
        thread::sleep(Duration::from_millis(20));
    };
    let stdout = stdout_reader
        .join()
        .map_err(|_| "Command output reader stopped unexpectedly".to_string())?;
    let stderr = stderr_reader
        .join()
        .map_err(|_| "Command error reader stopped unexpectedly".to_string())?;
    Ok(CommandOutput {
        status,
        stdout,
        stderr,
    })
}

fn read_limited(reader: impl Read) -> Vec<u8> {
    let mut output = Vec::new();
    let _ = reader
        .take(COMMAND_OUTPUT_LIMIT.saturating_add(1))
        .read_to_end(&mut output);
    output.truncate(COMMAND_OUTPUT_LIMIT as usize);
    output
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StreamKind {
    Stdout,
    Stderr,
}

enum StreamMessage {
    Data(StreamKind, Vec<u8>),
    Finished,
}

fn spawn_stream_reader(
    mut reader: impl Read + Send + 'static,
    kind: StreamKind,
    sender: mpsc::Sender<StreamMessage>,
) {
    thread::spawn(move || {
        let mut buffer = [0u8; 256];
        loop {
            match reader.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(count) => {
                    if sender
                        .send(StreamMessage::Data(kind, buffer[..count].to_vec()))
                        .is_err()
                    {
                        return;
                    }
                }
            }
        }
        let _ = sender.send(StreamMessage::Finished);
    });
}

fn drain_streams(
    receiver: &mpsc::Receiver<StreamMessage>,
    stdout: &mut Vec<u8>,
    stderr: &mut Vec<u8>,
) {
    while let Ok(message) = receiver.recv_timeout(Duration::from_millis(10)) {
        if let StreamMessage::Data(kind, data) = message {
            append_limited(
                match kind {
                    StreamKind::Stdout => stdout,
                    StreamKind::Stderr => stderr,
                },
                &data,
            );
        }
    }
}

fn append_limited(target: &mut Vec<u8>, data: &[u8]) {
    let remaining = (COMMAND_OUTPUT_LIMIT as usize).saturating_sub(target.len());
    target.extend_from_slice(&data[..data.len().min(remaining)]);
}

fn terminate(child: &mut Child) {
    let process_group = -(child.id() as i32);
    unsafe {
        nix::libc::kill(process_group, nix::libc::SIGKILL);
    }
    let _ = child.wait();
}

fn isolate_process_group(command: &mut Command) {
    command.process_group(0);
}

fn command_failure(prefix: &str, output: &CommandOutput) -> String {
    command_failure_parts(prefix, &output.stdout, &output.stderr)
}

fn command_failure_parts(prefix: &str, stdout: &[u8], stderr: &[u8]) -> String {
    let detail = [stderr, stdout]
        .into_iter()
        .map(|bytes| String::from_utf8_lossy(bytes))
        .map(|text| text.trim().to_owned())
        .find(|text| !text.is_empty())
        .unwrap_or_else(|| "No additional details were provided".into());
    format!(
        "{prefix}: {}",
        detail.lines().take(4).collect::<Vec<_>>().join(" ")
    )
}

fn find_command(command: &str) -> Option<PathBuf> {
    env::var_os("PATH").and_then(|path| {
        env::split_paths(&path)
            .map(|directory| directory.join(command))
            .find(|candidate| candidate.is_file())
    })
}

fn find_command_path(command: &Path) -> bool {
    if command
        .parent()
        .is_some_and(|parent| parent != Path::new(""))
    {
        command.is_file()
    } else {
        command
            .to_str()
            .and_then(find_command)
            .is_some_and(|path| path.is_file())
    }
}

fn wipe(bytes: &mut [u8]) {
    for byte in bytes {
        unsafe { std::ptr::write_volatile(byte, 0) };
    }
    compiler_fence(Ordering::SeqCst);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn executable(path: &Path, body: &str) {
        let staged = path.with_extension("staged");
        fs::write(&staged, body).unwrap();
        let mut permissions = fs::metadata(&staged).unwrap().permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(&staged, permissions).unwrap();
        fs::rename(staged, path).unwrap();
    }

    fn environment(temp: &tempfile::TempDir) -> NetworkEnvironment {
        NetworkEnvironment {
            gio: temp.path().join("gio"),
            secret_tool: Some(temp.path().join("secret-tool")),
            runtime_dir: temp.path().join("runtime"),
            shares_file: temp.path().join("config/network-shares.toml"),
        }
    }

    #[test]
    fn share_address_is_normalized_and_rejects_credentials() {
        let parsed = ShareAddress::parse("SERVER/Public/").unwrap();
        assert_eq!(parsed.uri, "smb://SERVER/Public");
        assert!(ShareAddress::parse("smb://user:pass@server/share").is_err());
        assert!(ShareAddress::parse("https://server/share").is_err());
        assert!(ShareAddress::parse("smb://server").is_err());
        assert!(ShareAddress::parse("smb://server/share/folder").is_err());
    }

    #[test]
    fn authentication_messages_are_not_mistaken_for_input_prompts() {
        assert!(!prompt_ready(
            "Password required for smb://nas/private\n",
            "Password"
        ));
        assert!(!prompt_ready(
            "Authentication for User at smb://nas/private",
            "User"
        ));
        assert!(!prompt_ready("Password required: ", "Password"));
        assert!(prompt_ready(
            "Authentication required\nUser [alice]: ",
            "User"
        ));
        assert!(prompt_ready("Password: ", "Password"));
    }

    #[test]
    fn mounted_shares_are_found_without_contacting_the_network() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("gvfs");
        fs::create_dir_all(root.join("smb-share:server=nas,share=documents,user=test")).unwrap();
        let mounted = discover_mounted(&root);
        assert_eq!(mounted.len(), 1);
        assert_eq!(mounted[0].0.uri, "smb://nas/documents");
        assert!(mounted[0]
            .1
            .ends_with("smb-share:server=nas,share=documents,user=test"));
    }

    #[test]
    fn mounted_share_matching_ignores_smb_server_and_share_case() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("gvfs");
        let mount = root.join("smb-share:server=10.27.27.168,share=dataserver,user=test");
        fs::create_dir_all(&mount).unwrap();
        let requested = ShareAddress::parse("smb://10.27.27.168/DataServer").unwrap();

        assert_eq!(find_mount_path(&root, &requested), Some(mount));
    }

    #[test]
    fn saved_share_metadata_is_atomic_and_contains_no_password() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("config/network-shares.toml");
        let saved = vec![SavedShare {
            uri: "smb://nas/docs".into(),
            username: "alice".into(),
            domain: "WORKGROUP".into(),
        }];
        save_saved(&path, &saved).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(!text.to_lowercase().contains("password"));
        let loaded = load_saved(&path).unwrap();
        assert_eq!(loaded[0].username, "alice");
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn discovery_merges_remote_saved_and_mounted_shares() {
        let temp = tempfile::tempdir().unwrap();
        let environment = environment(&temp);
        executable(
            &environment.gio,
            "#!/bin/sh\nif [ \"$3\" = \"smb://\" ]; then\n  printf 'smb://nas/\\n'\nelif [ \"$3\" = \"smb://nas/\" ]; then\n  printf 'smb://nas/public\\n'\nfi\n",
        );
        executable(
            &environment.secret_tool.clone().unwrap(),
            "#!/bin/sh\nexit 0\n",
        );
        save_saved(
            &environment.shares_file,
            &[SavedShare {
                uri: "smb://saved/private".into(),
                username: "alice".into(),
                domain: String::new(),
            }],
        )
        .unwrap();
        fs::create_dir_all(
            environment
                .gvfs_root()
                .join("smb-share:server=mounted,share=media"),
        )
        .unwrap();

        let shares = discover(&environment).unwrap();
        assert_eq!(shares.len(), 3);
        assert!(shares.iter().any(|share| share.saved));
        assert!(shares.iter().any(|share| share.discovered));
        assert!(shares.iter().any(|share| share.mount_path.is_some()));
    }

    #[test]
    fn authenticated_mount_uses_stdin_and_does_not_put_password_in_arguments() {
        let temp = tempfile::tempdir().unwrap();
        let environment = environment(&temp);
        let log = temp.path().join("arguments");
        let input = temp.path().join("input");
        executable(
            &environment.gio,
            &format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\nprintf 'User: '\nread user\nprintf 'Domain: '\nread domain\nprintf 'Password: '\nread password\nprintf '%s|%s|%s' \"$user\" \"$domain\" \"$password\" > '{}'\n",
                log.display(),
                input.display()
            ),
        );
        executable(
            &environment.secret_tool.clone().unwrap(),
            "#!/bin/sh\nexit 0\n",
        );
        let mut password = NetworkSecret::default();
        for character in "correct horse".chars() {
            password.push(character);
        }
        let outcome = perform(
            NetworkAction::Connect(ConnectRequest {
                address: ShareAddress::parse("smb://nas/private").unwrap(),
                auth: NetworkAuth::Password {
                    username: "alice".into(),
                    domain: "WORKGROUP".into(),
                    password,
                    remember: false,
                },
            }),
            &environment,
        )
        .unwrap();
        assert!(matches!(outcome, NetworkOutcome::Connected { .. }));
        assert!(!fs::read_to_string(log).unwrap().contains("correct horse"));
        assert_eq!(
            fs::read_to_string(input).unwrap(),
            "alice|WORKGROUP|correct horse"
        );
    }

    #[test]
    fn remembered_password_uses_secret_tool_stdin_and_separate_metadata() {
        let temp = tempfile::tempdir().unwrap();
        let environment = environment(&temp);
        let secret_input = temp.path().join("secret-input");
        let secret_args = temp.path().join("secret-args");
        executable(&environment.gio, "#!/bin/sh\nexit 0\n");
        executable(
            &environment.secret_tool.clone().unwrap(),
            &format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\ncat > '{}'\n",
                secret_args.display(),
                secret_input.display()
            ),
        );
        let mut password = NetworkSecret::default();
        for character in "not-on-disk-by-minfm".chars() {
            password.push(character);
        }
        perform(
            NetworkAction::Connect(ConnectRequest {
                address: ShareAddress::parse("smb://nas/private").unwrap(),
                auth: NetworkAuth::Password {
                    username: "alice".into(),
                    domain: String::new(),
                    password,
                    remember: true,
                },
            }),
            &environment,
        )
        .unwrap();
        assert_eq!(
            fs::read_to_string(secret_input).unwrap(),
            "not-on-disk-by-minfm"
        );
        assert!(!fs::read_to_string(secret_args)
            .unwrap()
            .contains("not-on-disk-by-minfm"));
        assert!(!fs::read_to_string(environment.shares_file)
            .unwrap()
            .contains("not-on-disk-by-minfm"));
    }

    #[test]
    fn saved_password_is_looked_up_and_never_placed_in_gio_arguments() {
        let temp = tempfile::tempdir().unwrap();
        let environment = environment(&temp);
        let gio_args = temp.path().join("gio-args");
        let gio_input = temp.path().join("gio-input");
        executable(
            &environment.gio,
            &format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\nprintf 'Password: '\nread password\nprintf '%s' \"$password\" > '{}'\n",
                gio_args.display(),
                gio_input.display()
            ),
        );
        executable(
            &environment.secret_tool.clone().unwrap(),
            "#!/bin/sh\nif [ \"$1\" = lookup ]; then printf 'saved-password'; exit 0; fi\nexit 1\n",
        );

        let outcome = perform(
            NetworkAction::Connect(ConnectRequest {
                address: ShareAddress::parse("smb://nas/private").unwrap(),
                auth: NetworkAuth::Saved {
                    username: "alice".into(),
                    domain: String::new(),
                },
            }),
            &environment,
        )
        .unwrap();

        assert!(matches!(outcome, NetworkOutcome::Connected { .. }));
        assert_eq!(fs::read_to_string(gio_input).unwrap(), "saved-password");
        assert!(!fs::read_to_string(gio_args)
            .unwrap()
            .contains("saved-password"));
    }

    #[test]
    fn missing_saved_password_requests_fresh_credentials() {
        let temp = tempfile::tempdir().unwrap();
        let environment = environment(&temp);
        executable(&environment.gio, "#!/bin/sh\nexit 99\n");
        executable(
            &environment.secret_tool.clone().unwrap(),
            "#!/bin/sh\nprintf 'not found' >&2\nexit 1\n",
        );
        let outcome = perform(
            NetworkAction::Connect(ConnectRequest {
                address: ShareAddress::parse("smb://nas/private").unwrap(),
                auth: NetworkAuth::Saved {
                    username: "alice".into(),
                    domain: "WORKGROUP".into(),
                },
            }),
            &environment,
        )
        .unwrap();
        assert!(matches!(
            outcome,
            NetworkOutcome::CredentialsRequired {
                username,
                domain,
                ..
            } if username == "alice" && domain == "WORKGROUP"
        ));
    }

    #[test]
    fn disconnect_and_forget_use_only_the_selected_share() {
        let temp = tempfile::tempdir().unwrap();
        let environment = environment(&temp);
        let gio_args = temp.path().join("gio-args");
        let secret_args = temp.path().join("secret-args");
        executable(
            &environment.gio,
            &format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\n",
                gio_args.display()
            ),
        );
        executable(
            &environment.secret_tool.clone().unwrap(),
            &format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\n",
                secret_args.display()
            ),
        );
        let address = ShareAddress::parse("smb://nas/private").unwrap();
        save_saved(
            &environment.shares_file,
            &[
                SavedShare {
                    uri: address.uri.clone(),
                    username: "alice".into(),
                    domain: String::new(),
                },
                SavedShare {
                    uri: "smb://other/keep".into(),
                    username: "bob".into(),
                    domain: String::new(),
                },
            ],
        )
        .unwrap();
        let share = NetworkShare {
            address,
            mount_path: Some(temp.path().join("mounted")),
            username: Some("alice".into()),
            domain: Some(String::new()),
            saved: true,
            discovered: false,
        };

        perform(NetworkAction::Disconnect(share.clone()), &environment).unwrap();
        let disconnect_args = fs::read_to_string(&gio_args).unwrap();
        assert!(disconnect_args.contains("--unmount"));
        assert!(disconnect_args.contains("smb://nas/private"));

        perform(NetworkAction::Forget(share), &environment).unwrap();
        let clear_args = fs::read_to_string(secret_args).unwrap();
        assert!(clear_args
            .lines()
            .next()
            .is_some_and(|line| line == "clear"));
        let remaining = load_saved(&environment.shares_file).unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].uri, "smb://other/keep");
    }

    #[test]
    fn command_timeout_terminates_a_stalled_child() {
        let temp = tempfile::tempdir().unwrap();
        let stalled = temp.path().join("stalled");
        executable(&stalled, "#!/bin/sh\nsleep 10\n");
        let command = Command::new(stalled);
        let started = Instant::now();
        let error = match run_command(command, Duration::from_millis(40)) {
            Err(error) => error,
            Ok(_) => panic!("stalled command unexpectedly completed"),
        };
        assert!(error.contains("timed out"));
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn network_secret_debug_output_is_redacted() {
        let mut secret = NetworkSecret::default();
        for character in "private".chars() {
            secret.push(character);
        }
        assert_eq!(format!("{secret:?}"), "NetworkSecret([REDACTED])");
    }

    #[test]
    fn secret_service_probe_distinguishes_no_match_from_no_service() {
        let temp = tempfile::tempdir().unwrap();
        let environment = environment(&temp);
        executable(
            &environment.secret_tool.clone().unwrap(),
            "#!/bin/sh\nexit 1\n",
        );
        assert!(secret_service_available(&environment));

        executable(
            &environment.secret_tool.clone().unwrap(),
            "#!/bin/sh\nprintf 'Secret Service unavailable' >&2\nexit 1\n",
        );
        assert!(!secret_service_available(&environment));
    }

    #[test]
    fn installer_contains_the_exact_samba_consent_prompt() {
        let installer = include_str!("../install.sh");
        assert!(installer.contains("Install the required packages for Samba functionality? [y/N]"));
        assert!(installer.contains("gvfs-smb"));
        assert!(installer.contains("gvfs-backends"));
        assert!(installer.contains("libsecret-tools"));
    }
}
