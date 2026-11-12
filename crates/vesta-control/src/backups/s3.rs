//! Bounded S3-compatible object storage with AWS Signature Version 4.
//! No credential or endpoint is included in diagnostics; redirects are not followed.
use super::store::{MAX_OBJECT_BYTES, ObjectInfo, Result, Store, StoreError, valid_key};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashSet},
    io::Read,
    net::IpAddr,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use url::{Host, Url};

const MAX_LIST_BYTES: usize = 2 * 1024 * 1024;
const MAX_TOKEN_BYTES: usize = 16 * 1024;
const RETRIES: usize = 3;

/// Static credentials (session tokens are not supported). Prefixes identify a
/// repository directory; `repo` and `repo/` both mean the namespace `repo/`.
pub struct S3Config {
    pub endpoint: Url,
    pub region: String,
    pub bucket: String,
    pub prefix: String,
    pub access_key_id: String,
    pub secret_access_key: String,
    pub path_style: bool,
}
impl std::fmt::Debug for S3Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("S3Config { [redacted] }")
    }
}

pub struct S3Store {
    config: S3Config,
    agent: ureq::Agent,
    origin: String,
    host: String,
    root: String,
}
impl std::fmt::Debug for S3Store {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("S3Store { [redacted] }")
    }
}
impl S3Store {
    pub fn new(mut config: S3Config) -> Result<Self> {
        let endpoint = &config.endpoint;
        let loopback = match endpoint.host() {
            Some(Host::Domain(name)) => name.eq_ignore_ascii_case("localhost"),
            Some(Host::Ipv4(ip)) => ip.is_loopback(),
            Some(Host::Ipv6(ip)) => ip.is_loopback(),
            None => false,
        };
        // Virtual-host addressing would change localhost into bucket.localhost,
        // which is not itself an assured loopback host. Local HTTP uses path style.
        if !(endpoint.scheme() == "https"
            || endpoint.scheme() == "http" && loopback && config.path_style)
            || endpoint.host().is_none()
            || !endpoint.username().is_empty()
            || endpoint.password().is_some()
            || endpoint.query().is_some()
            || endpoint.fragment().is_some()
            || endpoint.path() != "/"
            || !valid_bucket(&config.bucket)
            || config.region.is_empty()
            || config.region.len() > 64
            || !config
                .region
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            || config.access_key_id.is_empty()
            || config.access_key_id.len() > 128
            || !config
                .access_key_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
            || config.secret_access_key.is_empty()
            || config.secret_access_key.len() > 4096
        {
            return Err(StoreError::Invalid);
        }
        if !config.prefix.is_empty() {
            let prefix = config.prefix.strip_suffix('/').unwrap_or(&config.prefix);
            if !valid_key(prefix) {
                return Err(StoreError::Invalid);
            }
            config.prefix = format!("{prefix}/");
        }
        let mut base = config.endpoint.clone();
        let root = if config.path_style {
            format!("/{}/", config.bucket)
        } else {
            let Some(Host::Domain(domain)) = base.host() else {
                return Err(StoreError::Invalid);
            };
            let host = format!("{}.{}", config.bucket, domain);
            base.set_host(Some(&host))
                .map_err(|_| StoreError::Invalid)?;
            "/".to_owned()
        };
        let host = match base.host().ok_or(StoreError::Invalid)? {
            Host::Ipv6(ip) => format!("[{ip}]"),
            host => host.to_string(),
        };
        let host = match base.port() {
            Some(port) => format!("{host}:{port}"),
            None => host,
        };
        let origin = format!("{}://{host}", base.scheme());
        let agent = ureq::Agent::config_builder()
            .timeout_connect(Some(Duration::from_secs(10)))
            .timeout_global(Some(Duration::from_secs(120)))
            .max_redirects(0)
            .http_status_as_error(false)
            .proxy(None)
            .build()
            .new_agent();
        Ok(Self {
            config,
            agent,
            origin,
            host,
            root,
        })
    }

    fn object_path(&self, key: &str) -> Result<String> {
        if !valid_key(key) {
            return Err(StoreError::Invalid);
        }
        Ok(format!("{}{}{}", self.root, self.config.prefix, key))
    }

    // All supported operations are idempotent, including replace-PUT and DELETE.
    // Three retries after the initial attempt; fresh signing time on each attempt.
    fn request(
        &self,
        method: &str,
        path: &str,
        query: &[(String, String)],
        payload: &[u8],
        body_limit: usize,
    ) -> Result<(u16, Vec<u8>)> {
        let uri = encode(path, true);
        let query = canonical_query(query);
        let url = if query.is_empty() {
            format!("{}{uri}", self.origin)
        } else {
            format!("{}{uri}?{query}", self.origin)
        };
        let payload_hash = hash(payload);
        for attempt in 0..=RETRIES {
            let date = signing_time()?;
            let headers = BTreeMap::from([
                ("host".to_owned(), self.host.clone()),
                ("x-amz-content-sha256".to_owned(), payload_hash.clone()),
                ("x-amz-date".to_owned(), date.clone()),
            ]);
            let authorization = sign(
                &self.config,
                method,
                &uri,
                &query,
                &headers,
                &payload_hash,
                &date,
            );
            let request = ureq::http::Request::builder()
                .method(method)
                .uri(&url)
                .header("host", &self.host)
                .header("x-amz-content-sha256", &payload_hash)
                .header("x-amz-date", &date)
                .header("authorization", authorization)
                .body(payload)
                .map_err(|_| StoreError::Invalid)?;
            match self.agent.run(request) {
                Ok(mut response) => {
                    let status = response.status().as_u16();
                    if (500..600).contains(&status) && attempt < RETRIES {
                        drop(response);
                    } else if (200..300).contains(&status) && method == "GET" {
                        if response
                            .headers()
                            .get("content-length")
                            .and_then(|h| h.to_str().ok())
                            .and_then(|h| h.parse::<u64>().ok())
                            .is_some_and(|length| length > body_limit as u64)
                        {
                            return Err(StoreError::TooLarge);
                        }
                        let mut bytes = Vec::new();
                        match response
                            .body_mut()
                            .as_reader()
                            .take(body_limit as u64 + 1)
                            .read_to_end(&mut bytes)
                        {
                            Ok(_) if bytes.len() > body_limit => return Err(StoreError::TooLarge),
                            Ok(_) => return Ok((status, bytes)),
                            Err(_) if attempt < RETRIES => {}
                            Err(_) => return Err(StoreError::Unavailable),
                        }
                    } else {
                        return Ok((status, Vec::new()));
                    }
                }
                Err(error) if attempt < RETRIES && retryable(&error) => {}
                Err(_) => return Err(StoreError::Unavailable),
            }
            std::thread::sleep(Duration::from_millis(100 * (1 << attempt)));
        }
        Err(StoreError::Unavailable)
    }
}
fn retryable(error: &ureq::Error) -> bool {
    matches!(
        error,
        ureq::Error::Io(_)
            | ureq::Error::Timeout(_)
            | ureq::Error::HostNotFound
            | ureq::Error::ConnectionFailed
    )
}
fn valid_bucket(bucket: &str) -> bool {
    (3..=63).contains(&bucket.len())
        && bucket.parse::<IpAddr>().is_err()
        && bucket.split('.').all(|label| {
            !label.is_empty()
                && label.as_bytes()[0].is_ascii_alphanumeric()
                && label.as_bytes()[label.len() - 1].is_ascii_alphanumeric()
                && label
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        })
}
impl Store for S3Store {
    fn put(&self, key: &str, bytes: &[u8]) -> Result<()> {
        let path = self.object_path(key)?;
        if bytes.len() > MAX_OBJECT_BYTES {
            return Err(StoreError::TooLarge);
        }
        let (status, _) = self.request("PUT", &path, &[], bytes, 0)?;
        if (200..300).contains(&status) {
            Ok(())
        } else {
            Err(StoreError::Unavailable)
        }
    }
    fn get(&self, key: &str) -> Result<Vec<u8>> {
        let (status, body) =
            self.request("GET", &self.object_path(key)?, &[], &[], MAX_OBJECT_BYTES)?;
        match status {
            200 => Ok(body),
            404 => Err(StoreError::NotFound),
            _ => Err(StoreError::Unavailable),
        }
    }
    fn exists(&self, key: &str) -> Result<bool> {
        let (status, _) = self.request("HEAD", &self.object_path(key)?, &[], &[], 0)?;
        match status {
            200 => Ok(true),
            404 => Ok(false),
            _ => Err(StoreError::Unavailable),
        }
    }
    fn delete(&self, key: &str) -> Result<()> {
        let (status, _) = self.request("DELETE", &self.object_path(key)?, &[], &[], 0)?;
        match status {
            200 | 204 | 404 => Ok(()),
            _ => Err(StoreError::Unavailable),
        }
    }
    fn list(&self, prefix: &str, limit: usize) -> Result<Vec<ObjectInfo>> {
        if !prefix.is_empty() && !prefix.strip_suffix('/').is_some_and(valid_key) {
            return Err(StoreError::Invalid);
        }
        let wanted = format!("{}{prefix}", self.config.prefix);
        let mut result = Vec::new();
        let mut scanned = 0usize;
        let mut token: Option<String> = None;
        let mut seen = HashSet::new();
        loop {
            let mut query = vec![
                ("list-type".to_owned(), "2".to_owned()),
                ("max-keys".to_owned(), "1000".to_owned()),
                ("prefix".to_owned(), wanted.clone()),
            ];
            if let Some(token) = &token {
                query.push(("continuation-token".to_owned(), token.clone()));
            }
            let path = if self.root == "/" {
                "/"
            } else {
                self.root.trim_end_matches('/')
            };
            let (status, body) = self.request("GET", path, &query, &[], MAX_LIST_BYTES)?;
            if status != 200 {
                return Err(StoreError::Unavailable);
            }
            let page = parse_list(&body)?;
            scanned = scanned
                .checked_add(page.objects.len())
                .ok_or(StoreError::TooLarge)?;
            if scanned > limit {
                return Err(StoreError::TooLarge);
            }
            for object in &page.objects {
                if !object.key.starts_with(&wanted) {
                    continue;
                }
                let Some(key) = object.key.strip_prefix(&self.config.prefix) else {
                    continue;
                };
                if valid_key(key) {
                    result.push(ObjectInfo {
                        key: key.to_owned(),
                        size: object.size,
                        modified_ms: object.modified_ms,
                    });
                }
            }
            match page.next {
                None => return Ok(result),
                Some(next) => {
                    if page.objects.is_empty() || !seen.insert(next.clone()) {
                        return Err(StoreError::Invalid);
                    }
                    token = Some(next);
                }
            }
        }
    }
}

fn hash(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}
// RFC 2104: hash long keys before zero-padding to the SHA-256 block size.
fn hmac(key: &[u8], message: &[u8]) -> [u8; 32] {
    let mut block = [0u8; 64];
    if key.len() > block.len() {
        block[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        block[..key.len()].copy_from_slice(key);
    }
    let mut inner = Sha256::new();
    inner.update(block.map(|b| b ^ 0x36));
    inner.update(message);
    let mut outer = Sha256::new();
    outer.update(block.map(|b| b ^ 0x5c));
    outer.update(inner.finalize());
    outer.finalize().into()
}
fn encode(value: &str, slash: bool) -> String {
    let mut out = String::with_capacity(value.len());
    const HEX: &[u8] = b"0123456789ABCDEF";
    for b in value.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) || slash && b == b'/' {
            out.push(char::from(b));
        } else {
            out.push('%');
            out.push(char::from(HEX[(b >> 4) as usize]));
            out.push(char::from(HEX[(b & 15) as usize]));
        }
    }
    out
}
fn canonical_query(query: &[(String, String)]) -> String {
    let mut fields: Vec<_> = query
        .iter()
        .map(|(k, v)| (encode(k, false), encode(v, false)))
        .collect();
    fields.sort();
    fields
        .into_iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("&")
}
fn sign(
    config: &S3Config,
    method: &str,
    uri: &str,
    query: &str,
    headers: &BTreeMap<String, String>,
    payload_hash: &str,
    date: &str,
) -> String {
    let signed_headers = headers.keys().cloned().collect::<Vec<_>>().join(";");
    let canonical_headers = headers
        .iter()
        .map(|(k, v)| {
            format!(
                "{k}:{}\n",
                v.split_ascii_whitespace().collect::<Vec<_>>().join(" ")
            )
        })
        .collect::<String>();
    let canonical =
        format!("{method}\n{uri}\n{query}\n{canonical_headers}\n{signed_headers}\n{payload_hash}");
    let scope = format!("{}/{}/s3/aws4_request", &date[..8], config.region);
    let to_sign = format!(
        "AWS4-HMAC-SHA256\n{date}\n{scope}\n{}",
        hash(canonical.as_bytes())
    );
    let date_key = hmac(
        format!("AWS4{}", config.secret_access_key).as_bytes(),
        &date.as_bytes()[..8],
    );
    let region_key = hmac(&date_key, config.region.as_bytes());
    let service_key = hmac(&region_key, b"s3");
    let signing_key = hmac(&service_key, b"aws4_request");
    format!(
        "AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={signed_headers}, Signature={}",
        config.access_key_id,
        hex::encode(hmac(&signing_key, to_sign.as_bytes()))
    )
}

// Gregorian civil date conversion; bounded inputs, independent of local time zone.
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    (year + i64::from(month <= 2), month, day)
}
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = year - i64::from(month <= 2);
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = month + if month > 2 { -3 } else { 9 };
    let doy = (153 * mp + 2) / 5 + day - 1;
    era * 146097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719468
}
fn signing_time() -> Result<String> {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| StoreError::Unavailable)?
        .as_secs();
    if secs > 253402300799 {
        return Err(StoreError::Unavailable);
    }
    let (year, month, day) = civil_from_days((secs / 86400) as i64);
    Ok(format!(
        "{year:04}{month:02}{day:02}T{:02}{:02}{:02}Z",
        secs / 3600 % 24,
        secs / 60 % 60,
        secs % 60
    ))
}
fn parse_time(value: &str) -> Result<i64> {
    let b = value.as_bytes();
    if b.len() < 20
        || b[4] != b'-'
        || b[7] != b'-'
        || b[10] != b'T'
        || b[13] != b':'
        || b[16] != b':'
        || b.last() != Some(&b'Z')
    {
        return Err(StoreError::Invalid);
    }
    let number = |start: usize, end: usize| -> Result<i64> {
        let bytes = &b[start..end];
        if !bytes.iter().all(u8::is_ascii_digit) {
            return Err(StoreError::Invalid);
        }
        Ok(bytes.iter().fold(0, |v, b| v * 10 + i64::from(b - b'0')))
    };
    let (y, m, d) = (number(0, 4)?, number(5, 7)?, number(8, 10)?);
    let (h, min, sec) = (number(11, 13)?, number(14, 16)?, number(17, 19)?);
    let days = days_from_civil(y, m, d);
    if !(1..=12).contains(&m)
        || !(1..=31).contains(&d)
        || civil_from_days(days) != (y, m, d)
        || h > 23
        || min > 59
        || sec > 59
    {
        return Err(StoreError::Invalid);
    }
    let millis = if b.len() == 20 {
        0
    } else {
        if b[19] != b'.' || !(22..=30).contains(&b.len()) {
            return Err(StoreError::Invalid);
        }
        let digits = &b[20..b.len() - 1];
        if !digits.iter().all(u8::is_ascii_digit) {
            return Err(StoreError::Invalid);
        }
        digits
            .iter()
            .take(3)
            .fold(0, |v, b| v * 10 + i64::from(b - b'0'))
            * 10i64.pow(3u32.saturating_sub(digits.len() as u32))
    };
    Ok((days * 86400 + h * 3600 + min * 60 + sec) * 1000 + millis)
}

struct ListPage {
    objects: Vec<ObjectInfo>,
    next: Option<String>,
}
struct XmlNode {
    name: String,
    text: String,
    children: Vec<XmlNode>,
}
impl XmlNode {
    fn field(&self, name: &str) -> Result<Option<&str>> {
        let mut matches = self.children.iter().filter(|n| n.name == name);
        let node = matches.next();
        if matches.next().is_some() || node.is_some_and(|n| !n.children.is_empty()) {
            return Err(StoreError::Invalid);
        }
        Ok(node.map(|n| n.text.as_str()))
    }
    fn required(&self, name: &str) -> Result<&str> {
        self.field(name)?.ok_or(StoreError::Invalid)
    }
}
fn unescape(value: &str) -> Result<String> {
    let mut result = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(at) = rest.find('&') {
        result.push_str(&rest[..at]);
        rest = &rest[at..];
        let end = rest.find(';').ok_or(StoreError::Invalid)?;
        let entity = &rest[..=end];
        result.push(match entity {
            "&amp;" => '&',
            "&lt;" => '<',
            "&gt;" => '>',
            "&quot;" => '"',
            "&apos;" => '\'',
            _ => return Err(StoreError::Invalid),
        });
        rest = &rest[end + 1..];
    }
    result.push_str(rest);
    if result
        .chars()
        .any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t'))
    {
        return Err(StoreError::Invalid);
    }
    Ok(result)
}
// A bounded, deliberately small XML subset. No DTDs, external entities, CDATA,
// recursion, or entity expansion. The only attribute accepted is S3's xmlns.
fn parse_xml(body: &[u8]) -> Result<XmlNode> {
    if body.len() > MAX_LIST_BYTES {
        return Err(StoreError::TooLarge);
    }
    let text = std::str::from_utf8(body).map_err(|_| StoreError::Invalid)?;
    let mut rest = text.trim();
    if rest.starts_with("<?xml ") {
        let end = rest.find("?>").ok_or(StoreError::Invalid)?;
        rest = rest[end + 2..].trim_start();
    }
    let mut stack: Vec<XmlNode> = Vec::new();
    let mut root = None;
    let mut count = 0usize;
    while !rest.is_empty() {
        if !rest.starts_with('<') {
            let end = rest.find('<').unwrap_or(rest.len());
            let content = unescape(&rest[..end])?;
            if let Some(node) = stack.last_mut() {
                node.text.push_str(&content);
            } else if !content.trim().is_empty() {
                return Err(StoreError::Invalid);
            }
            rest = &rest[end..];
            continue;
        }
        let end = rest.find('>').ok_or(StoreError::Invalid)?;
        let tag = &rest[1..end];
        rest = &rest[end + 1..];
        if let Some(name) = tag.strip_prefix('/') {
            let node = stack.pop().ok_or(StoreError::Invalid)?;
            if node.name != name || !node.children.is_empty() && !node.text.trim().is_empty() {
                return Err(StoreError::Invalid);
            }
            if let Some(parent) = stack.last_mut() {
                parent.children.push(node);
            } else if root.replace(node).is_some() {
                return Err(StoreError::Invalid);
            }
        } else {
            let empty = tag.ends_with('/');
            let tag = tag.strip_suffix('/').unwrap_or(tag);
            let (name, attributes) = tag.split_once(char::is_whitespace).unwrap_or((tag, ""));
            if name.is_empty()
                || !name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
                || !(attributes.is_empty()
                    || stack.is_empty()
                        && name == "ListBucketResult"
                        && matches!(
                            attributes.trim(),
                            "xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\""
                                | "xmlns='http://s3.amazonaws.com/doc/2006-03-01/'"
                        ))
            {
                return Err(StoreError::Invalid);
            }
            count += 1;
            if count > 16000 || stack.len() >= 8 {
                return Err(StoreError::TooLarge);
            }
            let node = XmlNode {
                name: name.to_owned(),
                text: String::new(),
                children: Vec::new(),
            };
            if empty {
                if let Some(parent) = stack.last_mut() {
                    parent.children.push(node);
                } else if root.replace(node).is_some() {
                    return Err(StoreError::Invalid);
                }
            } else {
                stack.push(node);
            }
        }
    }
    if !stack.is_empty() {
        return Err(StoreError::Invalid);
    }
    root.ok_or(StoreError::Invalid)
}
fn parse_list(body: &[u8]) -> Result<ListPage> {
    let root = parse_xml(body)?;
    if root.name != "ListBucketResult" {
        return Err(StoreError::Invalid);
    }
    let truncated = match root.required("IsTruncated")?.trim() {
        "true" => true,
        "false" => false,
        _ => return Err(StoreError::Invalid),
    };
    let next = root.field("NextContinuationToken")?;
    let next = if truncated {
        let token = next.filter(|t| !t.is_empty()).ok_or(StoreError::Invalid)?;
        if token.len() > MAX_TOKEN_BYTES {
            return Err(StoreError::TooLarge);
        }
        Some(token.to_owned())
    } else {
        None
    };
    let mut objects = Vec::new();
    for node in root.children.iter().filter(|n| n.name == "Contents") {
        if objects.len() >= 1000 {
            return Err(StoreError::TooLarge);
        }
        let key = node.required("Key")?;
        if key.len() > 1024 {
            return Err(StoreError::Invalid);
        }
        let size = node.required("Size")?;
        if size.is_empty() || !size.bytes().all(|b| b.is_ascii_digit()) {
            return Err(StoreError::Invalid);
        }
        objects.push(ObjectInfo {
            key: key.to_owned(),
            size: size.parse().map_err(|_| StoreError::Invalid)?,
            modified_ms: parse_time(node.required("LastModified")?)?,
        });
    }
    Ok(ListPage { objects, next })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{BufRead, BufReader, Write},
        net::TcpListener,
        thread,
    };

    fn config(endpoint: &str) -> S3Config {
        S3Config {
            endpoint: Url::parse(endpoint).unwrap(),
            region: "us-east-1".to_owned(),
            bucket: "examplebucket".to_owned(),
            prefix: String::new(),
            access_key_id: "AKIAIOSFODNN7EXAMPLE".to_owned(),
            secret_access_key: "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY".to_owned(),
            path_style: true,
        }
    }

    #[test]
    fn hmac_rfc4231_all_vectors() {
        let cases = [
            (vec![0x0b; 20], b"Hi There".to_vec(), "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"),
            (b"Jefe".to_vec(), b"what do ya want for nothing?".to_vec(), "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"),
            (vec![0xaa; 20], vec![0xdd; 50], "773ea91e36800e46854db8ebd09181a72959098b3ef8c122d9635514ced565fe"),
            ((1..=25).collect::<Vec<u8>>(), vec![0xcd; 50], "82558a389a443c0ea4cc819899f2083a85f0faa3e578f8077a2e3ff46729665b"),
            (vec![0xaa; 131], b"Test Using Larger Than Block-Size Key - Hash Key First".to_vec(), "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54"),
            (vec![0xaa; 131], b"This is a test using a larger than block-size key and a larger than block-size data. The key needs to be hashed before being used by the HMAC algorithm.".to_vec(), "9b09ffa71b942fcb27635fbcd5b0e944bfdc63644f0713938a7f51535c3a35e2"),
        ];
        for (key, message, expected) in cases {
            assert_eq!(hex::encode(hmac(&key, &message)), expected);
        }
        assert_eq!(
            &hex::encode(hmac(&[0x0c; 20], b"Test With Truncation"))[..32],
            "a3b6167473100ee06e0c796c2955552b"
        );
    }
    #[test]
    fn aws_published_get_object_signature() {
        let config = config("https://s3.amazonaws.com");
        let date = "20130524T000000Z";
        let payload_hash = hash(b"");
        let headers = BTreeMap::from([
            (
                "host".to_owned(),
                "examplebucket.s3.amazonaws.com".to_owned(),
            ),
            ("range".to_owned(), "bytes=0-9".to_owned()),
            ("x-amz-content-sha256".to_owned(), payload_hash.clone()),
            ("x-amz-date".to_owned(), date.to_owned()),
        ]);
        let authorization = sign(
            &config,
            "GET",
            "/test.txt",
            "",
            &headers,
            &payload_hash,
            date,
        );
        assert!(
            authorization.ends_with(
                "Signature=f0e8bdb87c964420e857bd35b5d6ed310bd44f0170aba48dd91039c6036bdb41"
            ),
            "{authorization}"
        );
    }
    #[test]
    fn canonical_encoding_does_not_normalize_s3_paths() {
        assert_eq!(
            encode("/a//b/../é +%?~", true),
            "/a//b/../%C3%A9%20%2B%25%3F~"
        );
        assert_eq!(encode("a/b", false), "a%2Fb");
        let fields = [("z", "+/="), ("a", "two words"), ("a", ""), ("é", "x")]
            .map(|(k, v)| (k.to_owned(), v.to_owned()));
        assert_eq!(
            canonical_query(&fields),
            "%C3%A9=x&a=&a=two%20words&z=%2B%2F%3D"
        );
    }
    #[test]
    fn endpoints_buckets_prefixes_and_credentials_are_validated() {
        for endpoint in [
            "https://s3.amazonaws.com",
            "http://127.0.0.1:9000",
            "http://[::1]:9000",
            "http://localhost:9000",
        ] {
            assert!(S3Store::new(config(endpoint)).is_ok(), "{endpoint}");
        }
        for endpoint in [
            "http://s3.amazonaws.com",
            "http://192.168.1.2",
            "https://user:pass@example.com",
            "https://example.com/path",
            "https://example.com/?secret=x",
            "https://example.com/#secret",
            "ftp://example.com",
        ] {
            assert!(
                matches!(S3Store::new(config(endpoint)), Err(StoreError::Invalid)),
                "{endpoint}"
            );
        }
        for bucket in [
            "ab",
            "Uppercase",
            "-bucket",
            "bucket-",
            "a..b",
            "a.-b",
            "127.0.0.1",
            "bucket/name",
        ] {
            let mut c = config("https://example.com");
            c.bucket = bucket.to_owned();
            assert!(
                matches!(S3Store::new(c), Err(StoreError::Invalid)),
                "{bucket}"
            );
        }
        for prefix in ["/bad", "a//b", "../bad", "A", "a/./b", "/"] {
            let mut c = config("https://example.com");
            c.prefix = prefix.to_owned();
            assert!(
                matches!(S3Store::new(c), Err(StoreError::Invalid)),
                "{prefix}"
            );
        }
        for prefix in ["repo", "repo/"] {
            let mut c = config("https://example.com");
            c.prefix = prefix.to_owned();
            assert_eq!(
                S3Store::new(c).unwrap().object_path("chunks/a").unwrap(),
                "/examplebucket/repo/chunks/a"
            );
        }
        let mut c = config("https://s3.amazonaws.com:9443");
        c.path_style = false;
        let s = S3Store::new(c).unwrap();
        assert_eq!(s.host, "examplebucket.s3.amazonaws.com:9443");
        assert_eq!(s.object_path("test.txt").unwrap(), "/test.txt");
        let mut c = config("https://127.0.0.1");
        c.path_style = false;
        assert!(matches!(S3Store::new(c), Err(StoreError::Invalid)));
        let mut c = config("https://example.com");
        c.access_key_id = "bad\r\nheader".to_owned();
        assert!(matches!(S3Store::new(c), Err(StoreError::Invalid)));
        let mut c = config("https://example.com");
        c.region.clear();
        assert!(matches!(S3Store::new(c), Err(StoreError::Invalid)));
        let mut c = config("https://example.com");
        c.secret_access_key.clear();
        assert!(matches!(S3Store::new(c), Err(StoreError::Invalid)));
    }
    #[test]
    fn debug_never_contains_secrets_or_endpoints() {
        let c = config("https://secret-host.example");
        assert_eq!(format!("{c:?}"), "S3Config { [redacted] }");
        assert_eq!(
            format!("{:?}", S3Store::new(c).unwrap()),
            "S3Store { [redacted] }"
        );
    }
    fn contents(key: &str, size: u64) -> String {
        format!(
            "<Contents><Key>{key}</Key><Size>{size}</Size><LastModified>2024-02-29T12:34:56.789Z</LastModified></Contents>"
        )
    }
    fn page(objects: &str, token: Option<&str>) -> String {
        let next = token
            .map(|t| format!("<NextContinuationToken>{t}</NextContinuationToken>"))
            .unwrap_or_default();
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?><ListBucketResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"><IsTruncated>{}</IsTruncated>{objects}{next}</ListBucketResult>",
            token.is_some()
        )
    }
    #[test]
    fn xml_pages_entities_and_continuation() {
        let xml = page(
            &contents("repo/a&amp;&lt;&gt;&quot;&apos;", 42),
            Some("opaque+/=&amp;"),
        );
        let parsed = parse_list(xml.as_bytes()).unwrap();
        assert_eq!(parsed.next.as_deref(), Some("opaque+/=&"));
        assert_eq!(parsed.objects[0].key, "repo/a&<>\"'");
        assert_eq!(parsed.objects[0].size, 42);
        assert_eq!(parsed.objects[0].modified_ms, 1709210096789);
        assert!(
            parse_list(page("", None).as_bytes())
                .unwrap()
                .objects
                .is_empty()
        );
    }
    #[test]
    fn xml_malformed_and_bounded() {
        for xml in [
            "",
            "<ListBucketResult>",
            "<ListBucketResult><IsTruncated>true</IsTruncated></ListBucketResult>",
            "<!DOCTYPE x><ListBucketResult/>",
            "<ListBucketResult><IsTruncated>false</IsTruncated><IsTruncated>false</IsTruncated></ListBucketResult>",
            "<ListBucketResult><IsTruncated>false</IsTruncated></Wrong>",
            "<ListBucketResult><IsTruncated>false</IsTruncated><Contents><Key>&unknown;</Key></Contents></ListBucketResult>",
            "<ListBucketResult><IsTruncated>false</IsTruncated><Contents><Key>x</Key><Size>-1</Size><LastModified>2024-01-01T00:00:00Z</LastModified></Contents></ListBucketResult>",
            "<ListBucketResult><IsTruncated>false</IsTruncated><Contents><Key>x</Key><Key>y</Key><Size>1</Size><LastModified>2024-01-01T00:00:00Z</LastModified></Contents></ListBucketResult>",
        ] {
            assert!(parse_list(xml.as_bytes()).is_err(), "{xml}");
        }
        assert!(matches!(
            parse_list(&vec![b' '; MAX_LIST_BYTES + 1]),
            Err(StoreError::TooLarge)
        ));
        assert!(matches!(
            parse_list(page(&contents("x", 1).repeat(1001), None).as_bytes()),
            Err(StoreError::TooLarge)
        ));
        assert!(matches!(
            parse_list(page("", Some(&"a".repeat(MAX_TOKEN_BYTES + 1))).as_bytes()),
            Err(StoreError::TooLarge)
        ));
        assert!(matches!(
            parse_list(b"<a><a><a><a><a><a><a><a><a/></a></a></a></a></a></a></a></a>"),
            Err(StoreError::TooLarge)
        ));
        assert!(parse_list(b"\xff").is_err());
    }
    #[test]
    fn iso8601_time_is_strict_and_timezone_independent() {
        for (text, expected) in [
            ("1970-01-01T00:00:00Z", 0),
            ("1969-12-31T23:59:59.999Z", -1),
            ("2000-02-29T00:00:00.1Z", 951782400100),
            ("1970-01-01T00:00:00.123456Z", 123),
        ] {
            assert_eq!(parse_time(text).unwrap(), expected);
        }
        for bad in [
            "2023-02-29T00:00:00Z",
            "2024-04-31T00:00:00Z",
            "2024-01-01T24:00:00Z",
            "2024-01-01T00:00:60Z",
            "2024-01-01T00:00:00+00:00",
            "2024-01-01T00:00:00.Z",
            "éééé-01-01T00:00:00Z",
        ] {
            assert!(parse_time(bad).is_err(), "{bad}");
        }
        assert_eq!(civil_from_days(days_from_civil(2024, 2, 29)), (2024, 2, 29));
        assert_eq!(signing_time().unwrap().len(), 16);
    }

    struct Request {
        method: String,
        target: String,
        headers: BTreeMap<String, String>,
        body: Vec<u8>,
    }
    fn fake_server<F>(count: usize, mut respond: F) -> (String, thread::JoinHandle<()>)
    where
        F: FnMut(Request) -> (u16, Vec<u8>) + Send + 'static,
    {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let handle = thread::spawn(move || {
            for _ in 0..count {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let fields: Vec<_> = line.split_whitespace().collect();
                let (method, target) = (fields[0].to_owned(), fields[1].to_owned());
                let mut headers = BTreeMap::new();
                loop {
                    line.clear();
                    reader.read_line(&mut line).unwrap();
                    if line == "\r\n" {
                        break;
                    }
                    let (key, value) = line.trim_end().split_once(':').unwrap();
                    headers.insert(key.to_ascii_lowercase(), value.trim().to_owned());
                }
                let size = headers
                    .get("content-length")
                    .map(|s| s.parse::<usize>().unwrap())
                    .unwrap_or(0);
                let mut body = vec![0; size];
                reader.read_exact(&mut body).unwrap();
                assert_eq!(headers["x-amz-content-sha256"], hash(&body));
                let authorization = &headers["authorization"];
                assert!(
                    authorization.starts_with("AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/")
                );
                assert!(authorization.contains("/us-east-1/s3/aws4_request, SignedHeaders=host;x-amz-content-sha256;x-amz-date, Signature="));
                let signature = authorization.split("Signature=").nth(1).unwrap();
                assert_eq!(signature.len(), 64);
                assert!(signature.bytes().all(|b| b.is_ascii_hexdigit()));
                let signed = BTreeMap::from([
                    ("host".to_owned(), headers["host"].clone()),
                    (
                        "x-amz-content-sha256".to_owned(),
                        headers["x-amz-content-sha256"].clone(),
                    ),
                    ("x-amz-date".to_owned(), headers["x-amz-date"].clone()),
                ]);
                let (uri, query) = target.split_once('?').unwrap_or((&target, ""));
                assert_eq!(
                    authorization,
                    &sign(
                        &config("http://127.0.0.1"),
                        &method,
                        uri,
                        query,
                        &signed,
                        &hash(&body),
                        &headers["x-amz-date"]
                    )
                );
                let head = method == "HEAD";
                let (status, body) = respond(Request {
                    method,
                    target,
                    headers,
                    body,
                });
                write!(
                    stream,
                    "HTTP/1.1 {status} Response\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                )
                .unwrap();
                if !head {
                    stream.write_all(&body).unwrap();
                }
            }
        });
        (endpoint, handle)
    }
    #[test]
    fn fake_s3_round_trip_pagination_foreign_keys_and_missing() {
        let mut stored = None;
        let (endpoint, server) = fake_server(10, move |request| {
            assert!(!request.headers["authorization"].contains("EXAMPLEKEY"));
            match request.method.as_str() {
                "PUT" => {
                    assert_eq!(request.target, "/examplebucket/repo/config.json");
                    stored = Some(request.body);
                    (200, vec![])
                }
                "GET" if request.target.contains("list-type=2") => {
                    assert!(request.target.contains("max-keys=1000"));
                    assert!(request.target.contains("prefix=repo%2F"));
                    if request.target.contains("continuation-token=") {
                        assert!(
                            request
                                .target
                                .contains("continuation-token=token%2B%2F%3D%26")
                        );
                        (200, page(&contents("repo/chunks/b", 2), None).into_bytes())
                    } else {
                        (
                            200,
                            page(
                                &(contents("repo/config.json", 3)
                                    + &contents("other/foreign", 1)
                                    + &contents("repo/UPPER", 1)),
                                Some("token+/=&amp;"),
                            )
                            .into_bytes(),
                        )
                    }
                }
                "GET" => stored
                    .as_ref()
                    .map(|b| (200, b.clone()))
                    .unwrap_or((404, vec![])),
                "HEAD" => {
                    if stored.is_some() {
                        (200, vec![])
                    } else {
                        (404, vec![])
                    }
                }
                "DELETE" => {
                    if stored.take().is_some() {
                        (204, vec![])
                    } else {
                        (404, vec![])
                    }
                }
                _ => panic!("unexpected method"),
            }
        });
        let mut c = config(&endpoint);
        c.prefix = "repo".to_owned();
        let s = S3Store::new(c).unwrap();
        assert_eq!(s.get("config.json"), Err(StoreError::NotFound));
        assert!(!s.exists("config.json").unwrap());
        s.put("config.json", b"one").unwrap();
        s.put("config.json", b"two").unwrap();
        assert_eq!(s.get("config.json").unwrap(), b"two");
        assert!(s.exists("config.json").unwrap());
        let objects = s.list("", 4).unwrap();
        assert_eq!(
            objects.iter().map(|o| o.key.as_str()).collect::<Vec<_>>(),
            ["config.json", "chunks/b"]
        );
        s.delete("config.json").unwrap();
        s.delete("config.json").unwrap();
        server.join().unwrap();
    }
    #[test]
    fn foreign_objects_count_toward_list_scan_limit() {
        let (endpoint, server) = fake_server(1, |_| {
            (
                200,
                page(
                    &(contents("foreign", 0) + &contents("repo/INVALID", 0)),
                    None,
                )
                .into_bytes(),
            )
        });
        let mut c = config(&endpoint);
        c.prefix = "repo".to_owned();
        assert_eq!(
            S3Store::new(c).unwrap().list("", 1),
            Err(StoreError::TooLarge)
        );
        server.join().unwrap();
    }
    #[test]
    fn list_rejects_repeated_tokens_and_empty_truncated_pages() {
        let (endpoint, server) = fake_server(2, |_| {
            (200, page(&contents("x", 1), Some("same")).into_bytes())
        });
        assert_eq!(
            S3Store::new(config(&endpoint)).unwrap().list("", 10),
            Err(StoreError::Invalid)
        );
        server.join().unwrap();
        let (endpoint, server) = fake_server(1, |_| (200, page("", Some("next")).into_bytes()));
        assert_eq!(
            S3Store::new(config(&endpoint)).unwrap().list("", 10),
            Err(StoreError::Invalid)
        );
        server.join().unwrap();
    }
    #[test]
    fn retries_5xx_but_not_client_errors() {
        let mut attempts = 0;
        let (endpoint, server) = fake_server(4, move |_| {
            attempts += 1;
            (if attempts < 4 { 503 } else { 200 }, b"ok".to_vec())
        });
        assert_eq!(
            S3Store::new(config(&endpoint)).unwrap().get("x").unwrap(),
            b"ok"
        );
        server.join().unwrap();
        let (endpoint, server) = fake_server(1, |_| (403, b"secret error body".to_vec()));
        assert_eq!(
            S3Store::new(config(&endpoint)).unwrap().get("x"),
            Err(StoreError::Unavailable)
        );
        server.join().unwrap();
    }
    fn raw_server(responses: Vec<String>) -> (String, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let handle = thread::spawn(move || {
            for response in responses {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                loop {
                    line.clear();
                    assert!(reader.read_line(&mut line).unwrap() > 0);
                    if line == "\r\n" {
                        break;
                    }
                }
                stream.write_all(response.as_bytes()).unwrap();
            }
        });
        (endpoint, handle)
    }
    #[test]
    fn get_bounds_both_content_length_and_chunked_bodies() {
        let (endpoint, server) = raw_server(vec![format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            MAX_OBJECT_BYTES + 1
        )]);
        assert_eq!(
            S3Store::new(config(&endpoint)).unwrap().get("x"),
            Err(StoreError::TooLarge)
        );
        server.join().unwrap();
        let (endpoint, server) = raw_server(vec!["HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n4\r\ntest\r\n0\r\n\r\n".to_owned()]);
        let store = S3Store::new(config(&endpoint)).unwrap();
        assert_eq!(
            store.request("GET", "/examplebucket/x", &[], &[], 3),
            Err(StoreError::TooLarge)
        );
        server.join().unwrap();
        assert_eq!(
            store.put("x", &vec![0; MAX_OBJECT_BYTES + 1]),
            Err(StoreError::TooLarge)
        );
    }
    #[test]
    fn network_errors_retry_and_retries_are_bounded() {
        let (endpoint, server) = raw_server(vec![
            String::new(),
            String::new(),
            String::new(),
            "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok".to_owned(),
        ]);
        assert_eq!(
            S3Store::new(config(&endpoint)).unwrap().get("x").unwrap(),
            b"ok"
        );
        server.join().unwrap();
        let (endpoint, server) = fake_server(4, |_| (503, Vec::new()));
        assert_eq!(
            S3Store::new(config(&endpoint)).unwrap().get("x"),
            Err(StoreError::Unavailable)
        );
        server.join().unwrap();
    }
    #[test]
    fn redirects_do_not_forward_credentials() {
        let destination = TcpListener::bind("127.0.0.1:0").unwrap();
        destination.set_nonblocking(true).unwrap();
        let (endpoint, server) = raw_server(vec![format!(
            "HTTP/1.1 307 Redirect\r\nLocation: http://{}/steal\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            destination.local_addr().unwrap()
        )]);
        assert_eq!(
            S3Store::new(config(&endpoint)).unwrap().get("x"),
            Err(StoreError::Unavailable)
        );
        assert!(
            matches!(destination.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock)
        );
        server.join().unwrap();
    }
    #[test]
    fn invalid_keys_and_prefixes_do_not_make_requests() {
        let store = S3Store::new(config("http://127.0.0.1:1")).unwrap();
        assert_eq!(store.put("../x", b""), Err(StoreError::Invalid));
        assert_eq!(store.get("A"), Err(StoreError::Invalid));
        assert_eq!(store.exists(""), Err(StoreError::Invalid));
        assert_eq!(store.delete("a//b"), Err(StoreError::Invalid));
        assert_eq!(store.list("chunks", 1), Err(StoreError::Invalid));
        assert_eq!(store.list("../", 1), Err(StoreError::Invalid));
    }
}
