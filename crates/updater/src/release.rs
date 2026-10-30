//! The releases on GitHub.

use anyhow::{Result, anyhow, bail};
use serde::Deserialize;

use crate::{http, version};

/// The newest releases of the repository. This is not `releases/latest` on purpose: the release
/// called `runtime` (the Wine archive) sits in the same list, and whichever of the two was
/// published last would be "latest".
pub const RELEASES_URL: &str = "https://api.github.com/repos/hawawa4/merlin/releases?per_page=30";

/// The release file listing the SHA-256 of every other file in it.
const CHECKSUMS_ASSET: &str = "SHA256SUMS.txt";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Asset {
    pub name: String,
    pub url: String,
    pub size: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Release {
    /// The tag without its `v`: `0.6.0`.
    pub version: String,
    pub notes: String,
    pub page_url: String,
    pub assets: Vec<Asset>,
}

impl Release {
    pub fn asset(&self, name: &str) -> Option<&Asset> {
        self.assets.iter().find(|a| a.name == name)
    }
}

/// Asks GitHub for the highest published release.
pub fn latest() -> Result<Release> {
    let text = http::fetch_text(RELEASES_URL).map_err(|e| anyhow!("checking for updates: {e}"))?;
    parse_latest(&text)
}

#[derive(Deserialize)]
struct RawRelease {
    tag_name: String,
    #[serde(default)]
    body: Option<String>,
    html_url: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    assets: Vec<RawAsset>,
}

#[derive(Deserialize)]
struct RawAsset {
    name: String,
    browser_download_url: String,
    #[serde(default)]
    size: u64,
}

/// The highest version among the published releases whose tag is `vX.Y.Z`. Drafts,
/// pre-releases and other tags (`runtime`) are skipped.
pub fn parse_latest(json: &str) -> Result<Release> {
    let raw: Vec<RawRelease> =
        serde_json::from_str(json).map_err(|e| anyhow!("reading the release list: {e}"))?;
    let best = raw
        .into_iter()
        .filter(|r| !r.draft && !r.prerelease)
        .filter_map(|r| version::parse(&r.tag_name).map(|v| (v, r)))
        .max_by_key(|(v, _)| *v);
    let Some((v, r)) = best else {
        bail!("no release has been published yet");
    };
    Ok(Release {
        version: format!("{}.{}.{}", v[0], v[1], v[2]),
        notes: r.body.unwrap_or_default(),
        page_url: r.html_url,
        assets: r
            .assets
            .into_iter()
            .map(|a| Asset {
                name: a.name,
                url: a.browser_download_url,
                size: a.size,
            })
            .collect(),
    })
}

/// The SHA-256 the release lists for the file called `name`.
pub fn checksum_for(rel: &Release, name: &str) -> Result<String> {
    let sums = rel.asset(CHECKSUMS_ASSET).ok_or_else(|| {
        anyhow!("this release has no {CHECKSUMS_ASSET} file, so the download can't be verified")
    })?;
    parse_checksum(&http::fetch_text(&sums.url)?, name)
}

/// Finds `name` in `sha256sum` output: `<hex>  <name>`, where the name may carry a `./` (the
/// release is summed with `./*`) or a `*` binary marker.
fn parse_checksum(sums: &str, name: &str) -> Result<String> {
    for line in sums.lines() {
        let mut fields = line.split_whitespace();
        let (Some(hex), Some(file), None) = (fields.next(), fields.next(), fields.next()) else {
            continue;
        };
        let file = file.trim_start_matches('*');
        if file.strip_prefix("./").unwrap_or(file) != name {
            continue;
        }
        if hex.len() != 64 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            bail!("malformed checksum for {name}");
        }
        return Ok(hex.to_ascii_lowercase());
    }
    bail!("no checksum listed for {name}")
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIST: &str = r#"[
      {"tag_name":"runtime","html_url":"https://x/runtime","draft":false,"prerelease":false,"body":null,"assets":[]},
      {"tag_name":"v0.7.0-rc1","html_url":"https://x/rc","draft":false,"prerelease":true,"assets":[]},
      {"tag_name":"v0.9.0","html_url":"https://x/draft","draft":true,"prerelease":false,"assets":[]},
      {"tag_name":"v0.6.0","html_url":"https://x/0.6.0","draft":false,"prerelease":false,"body":"notes",
       "assets":[{"name":"SHA256SUMS.txt","browser_download_url":"https://x/sums","size":10}]},
      {"tag_name":"v0.10.0","html_url":"https://x/0.10.0","draft":false,"prerelease":false,"assets":[]}
    ]"#;

    #[test]
    fn picks_the_highest_published_version() {
        let r = parse_latest(LIST).unwrap();
        assert_eq!(r.version, "0.10.0");
        assert_eq!(r.page_url, "https://x/0.10.0");
    }

    #[test]
    fn carries_notes_and_assets() {
        let only = LIST.replace("v0.10.0", "runtime");
        let r = parse_latest(&only).unwrap();
        assert_eq!(r.version, "0.6.0");
        assert_eq!(r.notes, "notes");
        assert_eq!(r.asset("SHA256SUMS.txt").unwrap().size, 10);
        assert!(r.asset("nope").is_none());
    }

    #[test]
    fn nothing_published_is_an_error() {
        assert!(parse_latest("[]").is_err());
        assert!(parse_latest("not json").is_err());
    }

    #[test]
    fn finds_checksums_in_sha256sum_output() {
        let a = "a".repeat(64);
        let b = "B".repeat(64);
        let sums =
            format!("{a}  ./merlin_0.6.0_amd64.deb\n{b} *merlin-0.6.0-linux-amd64.tar.gz\n\n");
        assert_eq!(parse_checksum(&sums, "merlin_0.6.0_amd64.deb").unwrap(), a);
        assert_eq!(
            parse_checksum(&sums, "merlin-0.6.0-linux-amd64.tar.gz").unwrap(),
            "b".repeat(64)
        );
        assert!(parse_checksum(&sums, "other").is_err());
        assert!(parse_checksum("abc  file", "file").is_err());
    }
}
