//! Spotify sign-in (OAuth authorization code + PKCE). Only the refresh token is kept on disk,
//! in %APPDATA%\Spoty.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::Mutex;

/// Spotify's desktop client ID. The library endpoints Spoty reads only accept its tokens.
const CLIENT_ID: &str = "65b708073fc0480ea92a077233ca87bd";
const REDIRECT_URI: &str = "http://127.0.0.1:8898/login";
const SCOPES: &str = "streaming user-read-private user-library-read playlist-read-private playlist-read-collaborative";

const DONE_PAGE: &str = "<!doctype html><meta charset=utf-8><title>Spoty</title>\
<body style=\"font:16px system-ui;background:#121212;color:#eee;display:grid;place-items:center;height:90vh\">\
<p>Signed in. You can close this tab and go back to Spoty.</p>";

pub fn data_dir() -> PathBuf {
    let base = std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(|| ".".into());
    base.join("Spoty")
}

fn token_file() -> PathBuf {
    data_dir().join("refresh_token")
}

pub fn open_url(url: &str) {
    let _ = std::process::Command::new("rundll32").args(["url.dll,FileProtocolHandler", url]).spawn();
}

struct Token {
    access: String,
    refresh: String,
    expires_at: Instant,
}

pub struct Auth {
    http: reqwest::Client,
    token: Mutex<Token>,
}

impl Auth {
    /// Signs in silently with the saved refresh token, or returns `None` if there isn't a usable one.
    pub async fn restore(http: &reqwest::Client) -> Option<Auth> {
        let saved = std::fs::read_to_string(token_file()).ok()?;
        let token = refresh(http, saved.trim()).await.ok()?;
        Some(Auth { http: http.clone(), token: Mutex::new(token) })
    }

    /// Opens the browser for a full sign-in and waits for Spotify to redirect back.
    pub async fn login(http: &reqwest::Client) -> Result<Auth, String> {
        let verifier = URL_SAFE_NO_PAD.encode(random_bytes::<48>());
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        let state = URL_SAFE_NO_PAD.encode(random_bytes::<16>());

        let listener = tokio::net::TcpListener::bind("127.0.0.1:8898")
            .await
            .map_err(|e| format!("couldn't listen on port 8898: {e}"))?;
        let mut url = reqwest::Url::parse("https://accounts.spotify.com/authorize").unwrap();
        url.query_pairs_mut()
            .append_pair("client_id", CLIENT_ID)
            .append_pair("response_type", "code")
            .append_pair("redirect_uri", REDIRECT_URI)
            .append_pair("code_challenge_method", "S256")
            .append_pair("code_challenge", &challenge)
            .append_pair("scope", SCOPES)
            .append_pair("state", &state);
        open_url(url.as_str());

        let code = loop {
            let (mut sock, _) = listener.accept().await.map_err(|e| e.to_string())?;
            let mut buf = vec![0u8; 8192];
            let n = sock.read(&mut buf).await.unwrap_or(0);
            let request = String::from_utf8_lossy(&buf[..n]);
            let path = request.split_whitespace().nth(1).unwrap_or("");
            let Ok(redirect) = reqwest::Url::parse(&format!("http://127.0.0.1{path}")) else { continue };
            if redirect.path() != "/login" {
                let _ = sock.write_all(b"HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\n\r\n").await;
                continue;
            }
            let param = |k: &str| redirect.query_pairs().find(|(n, _)| n == k).map(|(_, v)| v.into_owned());
            let body = DONE_PAGE;
            let _ = sock
                .write_all(
                    format!("HTTP/1.1 200 OK\r\ncontent-type: text/html\r\ncontent-length: {}\r\n\r\n{body}", body.len())
                        .as_bytes(),
                )
                .await;
            if param("state").as_deref() != Some(state.as_str()) {
                return Err("the sign-in response didn't match this request".into());
            }
            match (param("code"), param("error")) {
                (Some(code), _) => break code,
                (_, Some(e)) => return Err(format!("Spotify said: {e}")),
                _ => return Err("no authorization code in the response".into()),
            }
        };

        let token = token_request(
            http,
            &[
                ("grant_type", "authorization_code"),
                ("code", &code),
                ("redirect_uri", REDIRECT_URI),
                ("client_id", CLIENT_ID),
                ("code_verifier", &verifier),
            ],
            "",
        )
        .await?;
        Ok(Auth { http: http.clone(), token: Mutex::new(token) })
    }

    pub fn forget() {
        let _ = std::fs::remove_file(token_file());
    }

    /// A valid access token, refreshed a minute before it expires.
    pub async fn access_token(&self) -> Result<String, String> {
        let mut token = self.token.lock().await;
        if token.expires_at <= Instant::now() + Duration::from_secs(60) {
            *token = refresh(&self.http, &token.refresh).await?;
        }
        Ok(token.access.clone())
    }
}

async fn refresh(http: &reqwest::Client, refresh_token: &str) -> Result<Token, String> {
    token_request(
        http,
        &[("grant_type", "refresh_token"), ("refresh_token", refresh_token), ("client_id", CLIENT_ID)],
        refresh_token,
    )
    .await
}

async fn token_request(http: &reqwest::Client, form: &[(&str, &str)], old_refresh: &str) -> Result<Token, String> {
    let resp = http
        .post("https://accounts.spotify.com/api/token")
        .form(form)
        .send()
        .await
        .map_err(|e| format!("couldn't reach Spotify: {e}"))?;
    let status = resp.status();
    let v: Value = resp.json().await.map_err(|e| e.to_string())?;
    if !status.is_success() {
        return Err(v["error_description"].as_str().or(v["error"].as_str()).unwrap_or("sign-in failed").to_string());
    }
    let access = v["access_token"].as_str().ok_or("no access token in the response")?.to_string();
    // Spotify may rotate the refresh token; keep the old one if it doesn't.
    let refresh = v["refresh_token"].as_str().unwrap_or(old_refresh).to_string();
    let _ = std::fs::create_dir_all(data_dir());
    let _ = std::fs::write(token_file(), &refresh);
    Ok(Token {
        access,
        refresh,
        expires_at: Instant::now() + Duration::from_secs(v["expires_in"].as_u64().unwrap_or(3600)),
    })
}

fn random_bytes<const N: usize>() -> [u8; N] {
    let mut b = [0u8; N];
    getrandom::fill(&mut b).expect("system random number generator");
    b
}
