//! Where the Linux system gets packages from — Alpine's `apk`, Python's
//! `pip` and Node's `npm` — each from its official source or a mirror; in
//! mainland China the official ones can be slow or unreachable. The lists
//! and the config each one needs are the reference app's (its Mirrors settings),
//! less JAIST's Alpine mirror (gone: 404 for the whole directory, checked
//! 2026-09-28) and with USTC's current pip address (the listed one only
//! redirects there). Names and regions are the reference app's.

use solos_api::{MirrorKind, MirrorSpeed, PackageMirror};
use std::time::{Duration, Instant};

/// Every source, official first within each kind.
pub fn all() -> Vec<PackageMirror> {
    use MirrorKind::*;
    [
        (Alpine, "official", "Official CDN", "https://dl-cdn.alpinelinux.org/alpine/", "Global"),
        (Alpine, "tuna", "Tsinghua TUNA", "https://mirrors.tuna.tsinghua.edu.cn/alpine/", "China"),
        (Alpine, "aliyun", "Alibaba", "https://mirrors.aliyun.com/alpine/", "China"),
        (Alpine, "ustc", "USTC", "https://mirrors.ustc.edu.cn/alpine/", "China"),
        (Alpine, "huawei", "Huawei", "https://repo.huaweicloud.com/alpine/", "China"),
        (Alpine, "tencent", "Tencent", "https://mirrors.cloud.tencent.com/alpine/", "China"),
        (Alpine, "leaseweb", "LEASEWEB UK", "https://mirror.leaseweb.com/alpine/", "Europe"),
        (Alpine, "rwth", "RWTH Germany", "https://ftp.halifax.rwth-aachen.de/alpine/", "Europe"),
        (Alpine, "kakao", "Kakao Korea", "https://mirror.kakao.com/alpine/", "Asia"),
        (Pip, "official", "Official PyPI", "https://pypi.org/simple/", "Global"),
        (Pip, "tuna", "Tsinghua TUNA", "https://pypi.tuna.tsinghua.edu.cn/simple/", "China"),
        (Pip, "aliyun", "Alibaba", "https://mirrors.aliyun.com/pypi/simple/", "China"),
        (Pip, "ustc", "USTC", "https://mirrors.ustc.edu.cn/pypi/simple/", "China"),
        (Pip, "huawei", "Huawei", "https://repo.huaweicloud.com/repository/pypi/simple/", "China"),
        (Pip, "tencent", "Tencent", "https://mirrors.cloud.tencent.com/pypi/simple/", "China"),
        (Npm, "official", "Official npm", "https://registry.npmjs.org/", "Global"),
        (Npm, "npmmirror", "npmmirror", "https://registry.npmmirror.com/", "China"),
        (Npm, "huawei", "Huawei", "https://repo.huaweicloud.com/repository/npm/", "China"),
        (Npm, "tencent", "Tencent", "https://mirrors.cloud.tencent.com/npm/", "China"),
    ]
    .into_iter()
    .map(|(kind, id, name, url, region)| PackageMirror { kind, id: id.into(), name: name.into(), url: url.into(), region: region.into() })
    .collect()
}

pub const OFFICIAL: &str = "official";

pub fn find(kind: MirrorKind, id: &str) -> Option<PackageMirror> {
    all().into_iter().find(|m| m.kind == kind && m.id == id)
}

pub fn kinds() -> [MirrorKind; 3] {
    [MirrorKind::Alpine, MirrorKind::Pip, MirrorKind::Npm]
}

/// The key a kind's choice is stored under.
pub fn key(kind: MirrorKind) -> &'static str {
    match kind {
        MirrorKind::Alpine => "alpine",
        MirrorKind::Pip => "pip",
        MirrorKind::Npm => "npm",
    }
}

/// The guest file a source goes into, and what it says. `branch` is the
/// guest's Alpine release (`v3.21`).
pub fn config(mirror: &PackageMirror, branch: &str) -> (&'static str, String) {
    let url = &mirror.url;
    match mirror.kind {
        MirrorKind::Alpine => ("/etc/apk/repositories", format!("{url}{branch}/main\n{url}{branch}/community\n")),
        MirrorKind::Pip => {
            let host = url.split("://").nth(1).and_then(|r| r.split('/').next()).unwrap_or_default();
            // `/etc/pip.conf`, not the reference app's `/etc/pip/pip.conf`:
            // pip reads the latter nowhere (`pip config debug` in this guest
            // lists /etc/xdg/pip/pip.conf and /etc/pip.conf), so a pip mirror
            // written there was silently not used (measured: the download
            // came from files.pythonhosted.org).
            ("/etc/pip.conf", format!("[global]\nindex-url = {url}\ntrusted-host = {host}\n"))
        }
        MirrorKind::Npm => ("/root/.npmrc", format!("registry={url}\n")),
    }
}

/// What a speed test asks each source for: what the package manager itself
/// asks first — the Alpine index for the guest's release, a package's page
/// on pip and npm. A registry's root is no measure: Huawei's pip answers it
/// 429 and Tencent's npm 404, while both serve packages (checked).
pub fn test_url(mirror: &PackageMirror, branch: &str) -> String {
    match mirror.kind {
        MirrorKind::Alpine => format!("{}{branch}/main/aarch64/APKINDEX.tar.gz", mirror.url),
        MirrorKind::Pip => format!("{}pip/", mirror.url),
        MirrorKind::Npm => format!("{}npm", mirror.url),
    }
}

/// How long each source takes to answer a HEAD request for `test_url`, all
/// at once, from the device (the guest's network is the device's); `None`
/// on an error status, a failure or more than `limit`. HEAD, as the
/// reference app times them: fetching the index itself from every mirror at
/// once ran each past the limit on a slow line (seen on screen), and a
/// registry's root is far larger. apk's User-Agent: two mirrors refuse
/// busybox wget's with 403 (measured).
pub async fn speed_test(mirrors: Vec<PackageMirror>, branch: &str, limit: Duration) -> Vec<MirrorSpeed> {
    crate::providers::install_crypto_provider();
    let client = match reqwest::Client::builder().user_agent("apk-tools/2.14").connect_timeout(limit).timeout(limit).build() {
        Ok(c) => c,
        Err(_) => return mirrors.into_iter().map(|m| MirrorSpeed { kind: m.kind, id: m.id, millis: None }).collect(),
    };
    let runs = mirrors.into_iter().map(|m| {
        let client = client.clone();
        let url = test_url(&m, branch);
        async move {
            let started = Instant::now();
            let ok = matches!(client.head(&url).send().await, Ok(r) if r.status().as_u16() < 400);
            MirrorSpeed { kind: m.kind, id: m.id, millis: ok.then(|| started.elapsed().as_millis() as u64) }
        }
    });
    futures::future::join_all(runs).await
}

/// For each kind, the fastest source that answered, if it is not the
/// official one: what a fresh Linux system switches to on its own, as the
/// reference app does. Official stays when it is fastest or nothing answered.
pub fn fastest_mirrors(speeds: &[MirrorSpeed]) -> Vec<(MirrorKind, String)> {
    kinds()
        .into_iter()
        .filter_map(|kind| {
            speeds
                .iter()
                .filter(|s| s.kind == kind)
                .filter_map(|s| s.millis.map(|ms| (ms, &s.id)))
                .min()
                .filter(|(_, id)| id.as_str() != OFFICIAL)
                .map(|(_, id)| (kind, id.clone()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_kind_has_the_reference_apps_sources_official_first() {
        let all = all();
        for (kind, count) in [(MirrorKind::Alpine, 9), (MirrorKind::Pip, 6), (MirrorKind::Npm, 4)] {
            let of: Vec<_> = all.iter().filter(|m| m.kind == kind).collect();
            assert_eq!(of.len(), count, "{kind:?}");
            assert_eq!(of[0].id, OFFICIAL);
            let mut ids: Vec<_> = of.iter().map(|m| &m.id).collect();
            ids.sort();
            ids.dedup();
            assert_eq!(ids.len(), count, "ids are unique within {kind:?}");
            assert!(of.iter().all(|m| m.url.starts_with("https://") && m.url.ends_with('/')));
        }
    }

    #[test]
    fn each_kind_writes_its_own_file() {
        let alpine = find(MirrorKind::Alpine, "aliyun").unwrap();
        assert_eq!(
            config(&alpine, "v3.21"),
            ("/etc/apk/repositories", "https://mirrors.aliyun.com/alpine/v3.21/main\nhttps://mirrors.aliyun.com/alpine/v3.21/community\n".into())
        );
        let pip = find(MirrorKind::Pip, "tuna").unwrap();
        assert_eq!(
            config(&pip, "v3.21"),
            ("/etc/pip.conf", "[global]\nindex-url = https://pypi.tuna.tsinghua.edu.cn/simple/\ntrusted-host = pypi.tuna.tsinghua.edu.cn\n".into())
        );
        let npm = find(MirrorKind::Npm, "npmmirror").unwrap();
        assert_eq!(config(&npm, "v3.21"), ("/root/.npmrc", "registry=https://registry.npmmirror.com/\n".into()));
        assert_eq!(test_url(&alpine, "v3.21"), "https://mirrors.aliyun.com/alpine/v3.21/main/aarch64/APKINDEX.tar.gz");
        assert_eq!(test_url(&pip, "v3.21"), "https://pypi.tuna.tsinghua.edu.cn/simple/pip/");
        assert_eq!(test_url(&npm, "v3.21"), "https://registry.npmmirror.com/npm");
    }

    #[test]
    fn a_fresh_system_moves_only_where_a_mirror_beat_the_official_source() {
        let s = |kind, id: &str, millis| MirrorSpeed { kind, id: id.into(), millis };
        let picked = fastest_mirrors(&[
            s(MirrorKind::Alpine, "official", Some(900)),
            s(MirrorKind::Alpine, "aliyun", Some(120)),
            s(MirrorKind::Alpine, "tuna", None),
            s(MirrorKind::Pip, "official", Some(80)),
            s(MirrorKind::Pip, "tuna", Some(300)),
            s(MirrorKind::Npm, "official", None),
            s(MirrorKind::Npm, "npmmirror", None),
        ]);
        assert_eq!(picked, vec![(MirrorKind::Alpine, "aliyun".to_string())]);
    }

    #[tokio::test]
    async fn a_source_that_answers_is_timed_and_one_that_refuses_or_is_gone_is_not() {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut sock = stream;
                let mut req = [0u8; 2048];
                let n = sock.read(&mut req).unwrap_or(0);
                let head = String::from_utf8_lossy(&req[..n]).to_string();
                let reply = if head.starts_with("HEAD /good/v3.21/main/aarch64/APKINDEX.tar.gz") && head.contains("apk-tools") {
                    "HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                } else {
                    "HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                };
                let _ = sock.write_all(reply.as_bytes());
            }
        });
        let m = |id: &str, url: String| PackageMirror { kind: MirrorKind::Alpine, id: id.into(), name: id.into(), url, region: "Global".into() };
        let speeds = speed_test(
            vec![m("good", format!("http://127.0.0.1:{port}/good/")), m("refuses", format!("http://127.0.0.1:{port}/refuses/")), m("gone", "http://127.0.0.1:1/gone/".into())],
            "v3.21",
            Duration::from_secs(5),
        )
        .await;
        let by = |id: &str| speeds.iter().find(|s| s.id == id).unwrap().millis;
        assert!(by("good").is_some());
        assert_eq!(by("refuses"), None);
        assert_eq!(by("gone"), None);
    }
}
