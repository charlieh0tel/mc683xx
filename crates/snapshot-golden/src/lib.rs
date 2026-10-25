//! Golden files of the snapshot formats (PLAN.md, State), for tests.
//! Each layer that saves has a test that saves a fixed state and
//! checks it here against `tests/golden/<name>.golden` in its crate:
//! the versions of the layers in the snapshot, its length and a hash.
//! Bytes that change under the same versions are a format change
//! without a version bump, and fail.  A bump regenerates the golden
//! with `UPDATE_GOLDEN=1 cargo test`; so two branches that change one
//! format conflict on its golden rather than merging silently.  A
//! bump of a layer nested in the snapshot bumps the snapshot's own
//! version too: its bytes changed, so its format did.  A change to a
//! test's fixture alone deletes the golden, then regenerates it.

use std::env;
use std::fmt::Write as _;
use std::fs;
use std::path::Path;

/// The variable that asks a run to write the goldens.
const UPDATE: &str = "UPDATE_GOLDEN";
/// Where in a crate the goldens are.
const DIRECTORY: &str = "tests/golden";
/// FNV-1a's 64-bit offset basis.
const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
/// FNV-1a's 64-bit prime.
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
/// Bytes of the version that starts every snapshot, little-endian
/// (`mc683xx_cpu32::cpu::state::encode`).
const VERSION_BYTES: usize = size_of::<u32>();
/// The line that names the versions.
const VERSIONS: &str = "versions";

/// FNV-1a, 64 bits, of `bytes`: a stable hash with no dependency.
fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(FNV_OFFSET, |hash, &byte| {
        (hash ^ u64::from(byte)).wrapping_mul(FNV_PRIME)
    })
}

/// The format version a snapshot starts with.
///
/// # Panics
///
/// If it is too short to hold one.
#[must_use]
pub fn version_of(snapshot: &[u8]) -> u32 {
    let (head, _) = snapshot
        .split_first_chunk::<VERSION_BYTES>()
        .expect("a snapshot holds a version");
    u32::from_le_bytes(*head)
}

/// The golden of `snapshot`, whose layers have `versions`.
fn render(versions: &[(&str, u32)], snapshot: &[u8]) -> String {
    let mut text = String::from(
        "# A snapshot golden (PLAN.md, State); generated, not edited.\n\
         # A format change bumps its VERSION, then UPDATE_GOLDEN=1 cargo test.\n",
    );
    text.push_str(VERSIONS);
    for (layer, version) in versions {
        // Writing to a string cannot fail.
        let _ = write!(text, " {layer}={version}");
    }
    let _ = write!(
        text,
        "\nlength {}\nfnv1a64 {:016x}\n",
        snapshot.len(),
        fnv1a(snapshot)
    );
    text
}

/// A golden's versions line.
fn versions_line(golden: &str) -> Option<&str> {
    golden.lines().find(|line| line.starts_with(VERSIONS))
}

/// A versions line's first entry: the snapshot's own layer.
fn own_version(line: &str) -> Option<&str> {
    line.split_whitespace().nth(1)
}

/// Checks `snapshot` against the golden `name`.
///
/// `versions` are its layers' versions, its own first, then those of
/// the snapshots inside it; the golden is in the crate at
/// `manifest_directory`.  With `UPDATE_GOLDEN` set it writes the
/// golden instead, unless the versions are the golden's.
///
/// # Panics
///
/// If the snapshot is not the golden's, if a nested layer's version
/// changed but not the snapshot's own, or if the golden cannot be
/// read or written.
pub fn check(manifest_directory: &str, name: &str, versions: &[(&str, u32)], snapshot: &[u8]) {
    let directory = Path::new(manifest_directory).join(DIRECTORY);
    let path = directory.join(format!("{name}.golden"));
    let expected = render(versions, snapshot);
    let found = fs::read_to_string(&path).ok();
    if found.as_deref() == Some(expected.as_str()) {
        return;
    }
    let same_versions = found
        .as_deref()
        .is_some_and(|golden| versions_line(golden) == versions_line(&expected));
    let bump = format!(
        "snapshot {name} changed under the same versions ({}): bump the VERSION \
         of the layer whose format changed, then regenerate its golden with \
         {UPDATE}=1 cargo test (a fixture change alone: delete {} first)",
        versions_line(&expected).unwrap_or_default(),
        path.display(),
    );
    let nested_only = found
        .as_deref()
        .and_then(versions_line)
        .is_some_and(|line| {
            !same_versions && own_version(line) == versions_line(&expected).and_then(own_version)
        });
    assert!(
        !nested_only,
        "snapshot {name}'s nested versions changed ({}) but not its own: bump \
         {name}'s VERSION too, then regenerate its golden with {UPDATE}=1 cargo test",
        versions_line(&expected).unwrap_or_default(),
    );
    if env::var_os(UPDATE).is_some() {
        assert!(!same_versions, "{bump}");
        fs::create_dir_all(&directory).expect("makes the golden directory");
        fs::write(&path, expected).expect("writes the golden");
        return;
    }
    assert!(
        found.is_some(),
        "no golden {}: run {UPDATE}=1 cargo test",
        path.display()
    );
    assert!(!same_versions, "{bump}");
    panic!(
        "snapshot {name}'s versions changed: regenerate its golden {} with {UPDATE}=1 cargo test",
        path.display()
    );
}

#[cfg(test)]
mod tests {
    use super::fnv1a;
    use super::own_version;
    use super::render;
    use super::version_of;
    use super::versions_line;

    #[test]
    fn fnv1a_matches_its_reference_values() {
        assert_eq!(fnv1a(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a(b"a"), 0xaf63_dc4c_8601_ec8c);
    }

    #[test]
    fn a_golden_names_the_versions_and_the_bytes() {
        let golden = render(&[("chip", 2), ("cpu", 5)], &[2, 0, 0, 0, 7]);
        assert_eq!(versions_line(&golden), Some("versions chip=2 cpu=5"));
        assert!(golden.contains("length 5\n"));
        assert_eq!(version_of(&[2, 0, 0, 0, 7]), 2);
        assert_eq!(versions_line(&golden).and_then(own_version), Some("chip=2"));
    }
}
