// The version stamped into the CLI when it is built: the commit, "-dirty" when tracked files
// have changes, and the time of the build. A copy of the code without .git names its commit in
// ONTOLOGIC_VERSION, which wins over git whenever it is set.
// Included by build.rs; read with env!("ONTOLOGIC_VERSION").

fn stamp_version() {
    println!("cargo:rerun-if-env-changed=ONTOLOGIC_VERSION");
    for path in ["src", ".git/HEAD", ".git/index"] {
        println!("cargo:rerun-if-changed={path}");
    }
    let run = |program: &str, args: &[&str]| {
        std::process::Command::new(program)
            .args(args)
            .output()
            .ok()
            .filter(|out| out.status.success())
            .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())
    };
    let commit = std::env::var("ONTOLOGIC_VERSION")
        .ok()
        .filter(|v| !v.is_empty())
        .or_else(|| {
            let sha = run("git", &["rev-parse", "--short", "HEAD"])?;
            let dirty = run("git", &["status", "--porcelain", "--untracked-files=no"])
                .is_some_and(|changes| !changes.is_empty());
            Some(match dirty {
                true => format!("{sha}-dirty"),
                false => sha,
            })
        })
        .unwrap_or_else(|| "unknown".into());
    let built = run("date", &["-u", "+%Y-%m-%d %H:%M UTC"]).unwrap_or_default();
    println!("cargo:rustc-env=ONTOLOGIC_VERSION={commit}, built {built}");
}
