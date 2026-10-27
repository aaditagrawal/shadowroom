use image::{Rgb, RgbImage};
use std::{
    fs,
    path::Path,
    process::{Command, Output},
};

fn run(folder: &Path, output: &Path, extra: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_shadowroom-compat-prototype"))
        .arg(folder)
        .args(["--offscreen"])
        .arg(output)
        .args(["--width", "640", "--height", "480", "--timeout", "20"])
        .args(extra)
        .env_remove("DISPLAY")
        .output()
        .unwrap()
}

fn succeeded(output: &Output) {
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("0 decode errors"));
}

#[test]
fn grid_and_loupe_render_deterministically_headless_without_modifying_originals() {
    let temporary = tempfile::tempdir().unwrap();
    let folder = temporary.path().join("photos");
    fs::create_dir(&folder).unwrap();
    let source = folder.join("test-photo.png");
    let image = RgbImage::from_fn(800, 600, |x, y| {
        Rgb([
            (x % 256) as u8,
            (y % 256) as u8,
            ((x / 8 + y / 8) % 256) as u8,
        ])
    });
    image.save(&source).unwrap();
    let original = fs::read(&source).unwrap();
    let modified = fs::metadata(&source).unwrap().modified().unwrap();
    let grid = temporary.path().join("grid.png");
    succeeded(&run(&folder, &grid, &[]));
    let grid_again = temporary.path().join("grid-again.png");
    succeeded(&run(&folder, &grid_again, &[]));
    assert_eq!(fs::read(&grid).unwrap(), fs::read(&grid_again).unwrap());
    let loupe = temporary.path().join("loupe.png");
    succeeded(&run(&folder, &loupe, &["--mode", "loupe"]));
    let actual = temporary.path().join("actual.png");
    succeeded(&run(
        &folder,
        &actual,
        &["--mode", "loupe", "--zoom", "100", "--pan", "80,-40"],
    ));
    assert_ne!(fs::read(&loupe).unwrap(), fs::read(&actual).unwrap());
    assert_eq!(image::open(&grid).unwrap().width(), 640);
    assert_eq!(image::open(&loupe).unwrap().height(), 480);
    assert_eq!(fs::read(&source).unwrap(), original);
    assert_eq!(fs::metadata(&source).unwrap().modified().unwrap(), modified);
    assert_eq!(fs::read_dir(&folder).unwrap().count(), 1);
    // Keep reproducible artifacts only when explicitly requested by the tester.
    if let Some(destination) = std::env::var_os("SHADOWROOM_COMPAT_TEST_ARTIFACTS") {
        fs::create_dir_all(&destination).unwrap();
        for (name, path) in [
            ("grid.png", grid),
            ("loupe-fit.png", loupe),
            ("loupe-actual-pan.png", actual),
        ] {
            fs::copy(path, Path::new(&destination).join(name)).unwrap();
        }
    }
}

#[test]
fn rejects_state_and_output_inside_source_and_never_overwrites_outputs() {
    let temporary = tempfile::tempdir().unwrap();
    let folder = temporary.path().join("photos");
    fs::create_dir(&folder).unwrap();
    let inside_state = folder.join("new-state/nested");
    let output = temporary.path().join("output.png");
    let failure = run(
        &folder,
        &output,
        &["--state-dir", inside_state.to_str().unwrap()],
    );
    assert!(!failure.status.success());
    assert!(!folder.join("new-state").exists());
    assert!(
        !run(&folder, &folder.join("output.png"), &[])
            .status
            .success()
    );
    assert!(!folder.join("output.png").exists());
    fs::write(&output, b"existing data").unwrap();
    assert!(!run(&folder, &output, &[]).status.success());
    assert_eq!(fs::read(&output).unwrap(), b"existing data");
    #[cfg(unix)]
    {
        let alias = temporary.path().join("source-alias");
        std::os::unix::fs::symlink(&folder, &alias).unwrap();
        let via_alias = alias.join("new-state");
        assert!(
            !run(
                &folder,
                &temporary.path().join("alias.png"),
                &["--state-dir", via_alias.to_str().unwrap()]
            )
            .status
            .success()
        );
        assert!(!folder.join("new-state").exists());
    }
}

#[test]
fn help_does_not_need_a_display_or_photo_folder() {
    let output = Command::new(env!("CARGO_BIN_EXE_shadowroom-compat-prototype"))
        .arg("--help")
        .env_remove("DISPLAY")
        .output()
        .unwrap();
    assert!(output.status.success());
    let help = String::from_utf8_lossy(&output.stdout);
    assert!(help.contains("PROTOTYPE"));
    assert!(help.contains("--offscreen"));
    assert!(help.contains("Originals are never edited"));
}
