//! Bounded, asynchronous GitHub requests; credentials never cross the Steel boundary.
use std::{collections::HashMap, path::PathBuf, sync::OnceLock, time::Duration};

use anyhow::{ensure, Context, Result};
use http_body_util::BodyExt;
use octocrab::Octocrab;
use serde::Deserialize;
use serde_json::{json, Value};

const LIMIT: usize = 1_048_576;
#[derive(Default)]
struct ClientPool {
    generation: u64,
    clients: HashMap<String, Octocrab>,
}
type Clients = parking_lot::Mutex<ClientPool>;

fn clients() -> &'static Clients {
    static CLIENTS: OnceLock<Clients> = OnceLock::new();
    CLIENTS.get_or_init(Default::default)
}

pub(super) fn clear_clients() {
    let mut pool = clients().lock();
    pool.generation += 1;
    pool.clients.clear();
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 255
        && name != "."
        && name != ".."
        && name
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"-_.".contains(&c))
}

fn valid_host(host: &str) -> bool {
    !host.is_empty()
        && host.len() <= 253
        && host.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || c == b'-')
        })
}

fn target(repo: &str, commit: &str) -> Result<(String, String)> {
    let parts = repo.split('/').collect::<Vec<_>>();
    ensure!(
        parts.len() == 3 && valid_host(parts[0]) && valid_name(parts[1]) && valid_name(parts[2]),
        "Invalid GitHub repository"
    );
    ensure!(
        matches!(commit.len(), 40 | 64) && commit.bytes().all(|c| c.is_ascii_hexdigit()),
        "Invalid commit hash"
    );
    let host = parts[0].to_ascii_lowercase();
    let base = api_base(&host);
    Ok((
        host,
        format!(
            "{base}/repos/{}/{}/commits/{commit}/pulls?per_page=20",
            parts[1], parts[2]
        ),
    ))
}

fn api_base(host: &str) -> String {
    if host == "github.com" {
        "https://api.github.com".into()
    } else if host.ends_with(".ghe.com") {
        format!("https://api.{host}")
    } else {
        format!("https://{host}/api/v3")
    }
}

fn token_variables(host: &str) -> [&'static str; 2] {
    if host == "github.com" || host.ends_with(".ghe.com") || host == "github.localhost" {
        ["GH_TOKEN", "GITHUB_TOKEN"]
    } else {
        ["GH_ENTERPRISE_TOKEN", "GITHUB_ENTERPRISE_TOKEN"]
    }
}

#[derive(Default, Deserialize)]
struct HostLogin {
    oauth_token: Option<String>,
    user: Option<String>,
    #[serde(default)]
    users: HashMap<String, Option<UserLogin>>,
}

#[derive(Default, Deserialize)]
struct UserLogin {
    oauth_token: Option<String>,
}

fn nonempty(value: Option<String>) -> Option<String> {
    value.filter(|value| !value.is_empty())
}

fn config_directory() -> Option<PathBuf> {
    for name in ["GH_CONFIG_DIR", "XDG_CONFIG_HOME"] {
        if let Some(path) = std::env::var_os(name).filter(|value| !value.is_empty()) {
            let path = PathBuf::from(path);
            return Some(if name == "GH_CONFIG_DIR" {
                path
            } else {
                path.join("gh")
            });
        }
    }
    #[cfg(windows)]
    if let Some(path) = std::env::var_os("AppData").filter(|value| !value.is_empty()) {
        return Some(PathBuf::from(path).join("GitHub CLI"));
    }
    helix_stdx::path::home_dir()
        .ok()
        .map(|path| path.join(".config/gh"))
}

fn read_login(bytes: &[u8], host: &str) -> Result<HostLogin> {
    let mut hosts: HashMap<String, HostLogin> = serde_yaml::from_slice(bytes)
        .map_err(|_| anyhow::anyhow!("Invalid GitHub login configuration"))?;
    Ok(hosts.remove(host).unwrap_or_default())
}

fn keyring_token(host: &str, user: &str) -> Option<String> {
    let service = format!("gh:{host}");
    #[cfg(target_os = "linux")]
    let entry = keyring::Entry::new_with_credential(Box::new(
        keyring::secret_service::SsCredential::new_with_no_target(&service, user).ok()?,
    ));
    #[cfg(windows)]
    let entry =
        keyring::Entry::new_with_target(&format!("{service}:{user}"), &service, user).ok()?;
    #[cfg(not(any(target_os = "linux", windows)))]
    let entry = keyring::Entry::new(&service, user).ok()?;
    nonempty(entry.get_password().ok())
}

fn login_token(
    host: &str,
    login: HostLogin,
    keyring: impl Fn(&str, &str) -> Option<String>,
) -> Option<String> {
    nonempty(login.oauth_token)
        .or_else(|| {
            login
                .user
                .as_ref()
                .and_then(|user| login.users.get(user))
                .and_then(|user| user.as_ref())
                .and_then(|user| nonempty(user.oauth_token.clone()))
        })
        .or_else(|| {
            login
                .user
                .as_deref()
                .filter(|user| !user.is_empty())
                .and_then(|user| keyring(host, user))
        })
        .or_else(|| keyring(host, ""))
}

fn credentials(host: &str) -> Result<Option<String>> {
    for name in token_variables(host) {
        if let Some(token) = nonempty(std::env::var(name).ok()) {
            return Ok(Some(token));
        }
    }
    let login = match config_directory() {
        Some(directory) => {
            use std::io::Read;
            match std::fs::File::open(directory.join("hosts.yml")) {
                Ok(file) => {
                    let mut bytes = Vec::new();
                    file.take((LIMIT + 1) as u64)
                        .read_to_end(&mut bytes)
                        .map_err(|_| anyhow::anyhow!("Cannot read GitHub login configuration"))?;
                    ensure!(bytes.len() <= LIMIT, "GitHub login configuration too large");
                    read_login(&bytes, host)?
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => HostLogin::default(),
                Err(_) => anyhow::bail!("Cannot read GitHub login configuration"),
            }
        }
        None => HostLogin::default(),
    };
    Ok(login_token(host, login, keyring_token))
}

fn new_client(token: Option<String>, base: &str) -> Result<Octocrab> {
    let builder = Octocrab::builder()
        .set_connect_timeout(Some(Duration::from_secs(3)))
        .set_read_timeout(Some(Duration::from_secs(5)));
    let builder = if let Some(token) = token {
        builder.personal_token(token)
    } else {
        builder
    };
    // Redirect and retry features deliberately disabled: never forward credentials
    // to a new authority or delay the UI with retry backoff.
    builder
        .base_uri(base)
        .map_err(|_| anyhow::anyhow!("Invalid GitHub API host"))?
        .build()
        .map_err(|_| anyhow::anyhow!("GitHub client initialization failed"))
}

async fn client(host: String) -> Result<Octocrab> {
    let generation = {
        let pool = clients().lock();
        if let Some(client) = pool.clients.get(&host).cloned() {
            return Ok(client);
        }
        pool.generation
    };
    tokio::task::spawn_blocking(move || {
        let client = new_client(credentials(&host)?, &api_base(&host))?;
        let mut pool = clients().lock();
        if generation != pool.generation {
            return Ok(client);
        }
        if pool.clients.len() >= 8 {
            pool.clients.clear();
        }
        Ok(pool.clients.entry(host).or_insert(client).clone())
    })
    .await
    .context("GitHub credential worker failed")?
}

async fn request(client: &Octocrab, url: &str) -> Result<Value> {
    let response = client
        ._get(url)
        .await
        .map_err(|_| anyhow::anyhow!("GitHub connection failed"))?;
    let status = response.status();
    let mut body = response.into_body();
    let mut bytes = Vec::new();
    while let Some(frame) = body.frame().await {
        let frame = frame.map_err(|_| anyhow::anyhow!("GitHub response failed"))?;
        if let Ok(data) = frame.into_data() {
            ensure!(
                bytes.len() + data.len() <= LIMIT,
                "GitHub response too large"
            );
            bytes.extend_from_slice(&data);
        }
    }
    let data: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    if !status.is_success() {
        return Ok(json!({"ok": false, "status": status.as_u16(),
            "error": match status.as_u16() { 401 | 403 => "GitHub authentication, permissions or rate limit failure", 429 => "GitHub rate limit exceeded", 300..=399 => "GitHub redirect rejected", _ => "GitHub API lookup failed" },
            "unpublished": matches!(status.as_u16(), 404 | 422) && data["message"].as_str().is_some_and(|message| message.starts_with("No commit found for SHA:"))}));
    }
    let pulls = data.as_array().context("Invalid GitHub PR response")?;
    ensure!(pulls.len() <= 20, "Too many GitHub PRs");
    let pulls = pulls.iter().map(|pr| json!({"number": pr["number"].as_u64().map(|value| value.to_string()),
        "title": pr["title"], "html_url": pr["html_url"], "state": pr["state"], "merged_at": pr["merged_at"]})).collect::<Vec<_>>();
    Ok(json!({"ok": true, "pulls": pulls}))
}

pub(super) async fn lookup(repo: String, commit: String) -> Value {
    let result = tokio::time::timeout(Duration::from_secs(5), async {
        let (host, url) = target(&repo, &commit)?;
        request(&client(host).await?, &url).await
    })
    .await;
    match result {
        Ok(Ok(value)) => value,
        // Never return raw HTTP/credential/YAML errors: they can contain secrets.
        Ok(Err(_)) => {
            json!({"ok": false, "error": "GitHub lookup unavailable (credentials, connection, or response)"})
        }
        Err(_) => json!({"ok": false, "error": "GitHub lookup timed out"}),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHA: &str = "0123456789abcdef0123456789abcdef01234567";

    #[test]
    fn routes_are_host_scoped_and_cannot_inject_a_url() {
        assert_eq!(
            target("github.com/o/r", SHA).unwrap().1,
            format!("https://api.github.com/repos/o/r/commits/{SHA}/pulls?per_page=20")
        );
        assert_eq!(
            target("THE.COMPANY.DEV/o/r", SHA).unwrap(),
            (
                "the.company.dev".into(),
                format!("https://the.company.dev/api/v3/repos/o/r/commits/{SHA}/pulls?per_page=20")
            )
        );
        assert!(target("tenant.ghe.com/o/r", SHA)
            .unwrap()
            .1
            .starts_with("https://api.tenant.ghe.com/"));
        for repo in [
            "github.com/o/..",
            "user:secret@host/o/r",
            "-host/o/r",
            "host/o/r?x",
            "host/o/r/extra",
            "host:443/o/r",
        ] {
            assert!(target(repo, SHA).is_err(), "{repo}");
        }
        assert!(target("github.com/o/r", "abc/../../user").is_err());
        assert_eq!(token_variables("github.com"), ["GH_TOKEN", "GITHUB_TOKEN"]);
        assert_eq!(
            token_variables("the.company.dev"),
            ["GH_ENTERPRISE_TOKEN", "GITHUB_ENTERPRISE_TOKEN"]
        );
    }

    #[test]
    fn saved_logins_and_native_keyring_fallback_are_host_and_user_scoped() {
        let yaml = b"github.com:\n  oauth_token: public-test-token\nthe.company.dev:\n  user: alice\n  users:\n    alice:\n      oauth_token: enterprise-test-token\nother.dev:\n  user: bob\n";
        let no_keyring = |_: &str, _: &str| None;
        assert_eq!(
            login_token(
                "github.com",
                read_login(yaml, "github.com").unwrap(),
                no_keyring
            )
            .as_deref(),
            Some("public-test-token")
        );
        assert_eq!(
            login_token(
                "the.company.dev",
                read_login(yaml, "the.company.dev").unwrap(),
                no_keyring
            )
            .as_deref(),
            Some("enterprise-test-token")
        );
        let keyed = |host: &str, user: &str| {
            assert_eq!(host, "other.dev");
            assert_eq!(user, "bob");
            Some("keyring-test-token".into())
        };
        assert_eq!(
            login_token("other.dev", read_login(yaml, "other.dev").unwrap(), keyed).as_deref(),
            Some("keyring-test-token")
        );
        let legacy = |_: &str, user: &str| {
            if user.is_empty() {
                Some("legacy-test-token".into())
            } else {
                None
            }
        };
        assert_eq!(
            login_token("other.dev", read_login(yaml, "other.dev").unwrap(), legacy).as_deref(),
            Some("legacy-test-token")
        );
        assert!(read_login(b"[bad yaml", "github.com").is_err());
        let null_user = read_login(
            b"other.dev:\n  user: bob\n  users:\n    bob:\n",
            "other.dev",
        )
        .unwrap();
        assert_eq!(
            login_token("other.dev", null_user, legacy).as_deref(),
            Some("legacy-test-token")
        );
    }

    async fn server(
        status: &str,
        headers: &str,
        body: String,
    ) -> (String, tokio::task::JoinHandle<String>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let response = format!("HTTP/1.1 {status}\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n{headers}\r\n{body}", body.len());
        let task = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut buffer = [0; 1024];
            while !request.ends_with(b"\r\n\r\n") {
                let count = stream.read(&mut buffer).await.unwrap();
                if count == 0 {
                    break;
                }
                request.extend_from_slice(&buffer[..count]);
            }
            let _ = stream.write_all(response.as_bytes()).await;
            String::from_utf8(request).unwrap()
        });
        (
            format!("http://{address}/api/v3/repos/o/r/commits/{SHA}/pulls?per_page=20"),
            task,
        )
    }

    #[tokio::test]
    async fn octocrab_returns_compact_prs_without_a_subprocess() {
        let (url, task) = server("200 OK", "", r#"[{"number":123,"title":"Fix","html_url":"https://the.company.dev/o/r/pull/123","state":"closed","merged_at":null}]"#.into()).await;
        let client = new_client(Some("synthetic-test-token".into()), &url).unwrap();
        let result = request(&client, &url).await.unwrap();
        assert_eq!(result["ok"], true);
        assert_eq!(result["pulls"][0]["number"], "123");
        let headers = task.await.unwrap();
        assert!(headers.starts_with(&format!(
            "GET /api/v3/repos/o/r/commits/{SHA}/pulls?per_page=20 "
        )));
        assert!(headers
            .to_ascii_lowercase()
            .contains("authorization: bearer synthetic-test-token"));
    }

    #[tokio::test]
    async fn redirects_errors_and_oversize_responses_are_safe() {
        let client = new_client(None, "https://api.github.com").unwrap();
        let (url, task) = server(
            "302 Found",
            "Location: http://127.0.0.1:1/steal\r\n",
            "{}".into(),
        )
        .await;
        assert_eq!(request(&client, &url).await.unwrap()["status"], 302);
        task.await.unwrap();
        let (url, task) = server(
            "401 Unauthorized",
            "",
            r#"{"message":"private-test-secret"}"#.into(),
        )
        .await;
        let result = request(&client, &url).await.unwrap();
        assert_eq!(result["ok"], false);
        assert!(!result.to_string().contains("private-test-secret"));
        task.await.unwrap();
        let (url, task) = server(
            "422 Unprocessable Entity",
            "",
            r#"{"message":"No commit found for SHA: missing"}"#.into(),
        )
        .await;
        assert_eq!(request(&client, &url).await.unwrap()["unpublished"], true);
        task.await.unwrap();
        let (url, task) = server("200 OK", "", "x".repeat(LIMIT + 1)).await;
        assert!(request(&client, &url).await.is_err());
        task.await.unwrap();
    }
}
