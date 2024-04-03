use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::str::FromStr;

use ecow::eco_format;
use typst::diag::{bail, PackageError, PackageResult, StrResult};
use typst::syntax::package::{PackageInfo, PackageSpec, PackageVersion, VersionlessPackageSpec};
use ureq::Response;

const HOST: &str = "https://packages.typst.org";

/// Make a package available in the on-disk cache.
pub fn prepare_package(root: &str, spec: &PackageSpec) -> PackageResult<PathBuf> {
    let subdir = format!(
        "typst/packages/{}/{}/{}",
        spec.namespace, spec.name, spec.version
    );

    if let Ok(data_dir) = PathBuf::from_str(&format!("{root}/packages")) {
        let dir = data_dir.join(&subdir);
        if dir.exists() {
            return Ok(dir);
        }
    }

    if let Ok(cache_dir) = PathBuf::from_str(&format!("{root}/packages")) {
        let dir = cache_dir.join(&subdir);
        if dir.exists() {
            return Ok(dir);
        }

        // Download from network if it doesn't exist yet.
        if spec.namespace == "preview" {
            download_package(spec, &dir)?;
            if dir.exists() {
                return Ok(dir);
            }
        }
    }

    Err(PackageError::NotFound(spec.clone()))
}

/// Try to determine the latest version of a package.
pub fn determine_latest_version(spec: &VersionlessPackageSpec) -> StrResult<PackageVersion> {
    if spec.namespace == "preview" {
        // For `@preview`, download the package index and find the latest
        // version.
        download_index()?
            .iter()
            .filter(|package| package.name == spec.name)
            .map(|package| package.version)
            .max()
            .ok_or_else(|| eco_format!("failed to find package {spec}"))
    } else {
        // For other namespaces, search locally. We only search in the data
        // directory and not the cache directory, because the latter is not
        // intended for storage of local packages.
        let subdir = format!("typst/packages/{}/{}", spec.namespace, spec.name);
        dirs::data_dir()
            .into_iter()
            .flat_map(|dir| std::fs::read_dir(dir.join(&subdir)).ok())
            .flatten()
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .filter_map(|path| path.file_name()?.to_string_lossy().parse().ok())
            .max()
            .ok_or_else(|| eco_format!("please specify the desired version"))
    }
}

/// Download a package over the network.
fn download_package(spec: &PackageSpec, package_dir: &Path) -> PackageResult<()> {
    // The `@preview` namespace is the only namespace that supports on-demand
    // fetching.
    assert_eq!(spec.namespace, "preview");

    let url = format!("{HOST}/preview/{}-{}.tar.gz", spec.name, spec.version);

    let data = match download(&url) {
        Ok(data) => data,
        Err(ureq::Error::Status(404, _)) => return Err(PackageError::NotFound(spec.clone())),
        Err(err) => return Err(PackageError::NetworkFailed(Some(eco_format!("{err}")))),
    };
    let mut body = vec![];
    let _ = data.into_reader().read_to_end(&mut body);
    let decompressed = flate2::read::GzDecoder::new(body.as_slice());
    println!("unpacking to {package_dir:?}");
    tar::Archive::new(decompressed)
        .unpack(package_dir)
        .map_err(|err| {
            fs::remove_dir_all(package_dir).ok();
            PackageError::MalformedArchive(Some(eco_format!("{err}")))
        })
}

/// Download the `@preview` package index.
fn download_index() -> StrResult<Vec<PackageInfo>> {
    let url = format!("{HOST}/preview/index.json");
    match download(&url) {
        Ok(response) => response
            .into_json()
            .map_err(|err| eco_format!("failed to parse package index: {err}")),
        Err(ureq::Error::Status(404, _)) => {
            bail!("failed to fetch package index (not found)")
        }
        Err(err) => bail!("failed to fetch package index ({err})"),
    }
}

fn download(url: &String) -> Result<Response, ureq::Error> {
    ureq::get(url).call()
}
