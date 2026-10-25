//! With the `musashi` feature, builds Musashi from `MUSASHI_DIR` (by
//! default `external/musashi`, which `make musashi` fetches): its
//! generator first, run on the host to write the opcode handlers, then
//! the core.  Without the feature, nothing.

fn main() {
    #[cfg(feature = "musashi")]
    musashi::build();
}

#[cfg(feature = "musashi")]
mod musashi {
    use std::env;
    use std::path::Path;
    use std::path::PathBuf;
    use std::process::Command;

    /// Where Musashi's source is, if `MUSASHI_DIR` does not say.
    const DEFAULT_SOURCE: &str = "../../external/musashi";

    /// Configuration switches (m68kconf.h) turned off: the PMMU, which
    /// the 68020 lacks.  Address errors are off by default (Musashi
    /// models them for the 68000 only), as are the FPU's callbacks; its
    /// code and `SoftFloat` are compiled only because Musashi's headers
    /// require them.  No differential run reaches them: the core takes
    /// line F as an exception, which is not compared.
    const OFF: [&str; 1] = ["M68K_EMULATE_PMMU"];

    pub(super) fn build() {
        println!("cargo::rerun-if-env-changed=MUSASHI_DIR");
        let manifest = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("set by cargo"));
        let source =
            env::var_os("MUSASHI_DIR").map_or_else(|| manifest.join(DEFAULT_SOURCE), PathBuf::from);
        assert!(
            source.join("m68kcpu.c").is_file(),
            "no Musashi source at {}: run `make musashi`, or set MUSASHI_DIR",
            source.display()
        );
        println!("cargo::rerun-if-changed={}", source.display());
        let out = PathBuf::from(env::var_os("OUT_DIR").expect("set by cargo"));
        generate(&source, &out);
        let mut build = cc::Build::new();
        build
            .include(&out)
            .include(&source)
            .file(source.join("m68kcpu.c"))
            .file(out.join("m68kops.c"))
            .file(source.join("softfloat/softfloat.c"))
            .warnings(false);
        for switch in OFF {
            build.define(switch, "0");
        }
        build.compile("musashi");
    }

    /// Builds `m68kmake` for the host and runs it, writing `m68kops.c`
    /// and `m68kops.h` into `out`.
    fn generate(source: &Path, out: &Path) {
        let host = env::var("HOST").expect("set by cargo");
        let generator = out.join("m68kmake");
        let compiler = cc::Build::new()
            .host(&host)
            .target(&host)
            .warnings(false)
            .get_compiler();
        let status = compiler
            .to_command()
            .arg(source.join("m68kmake.c"))
            .arg("-o")
            .arg(&generator)
            .status()
            .expect("run the C compiler");
        assert!(status.success(), "building m68kmake failed");
        let status = Command::new(&generator)
            .arg(out)
            .arg(source.join("m68k_in.c"))
            .current_dir(out)
            .status()
            .expect("run m68kmake");
        assert!(status.success(), "m68kmake failed");
    }
}
