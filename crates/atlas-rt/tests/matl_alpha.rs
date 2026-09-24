use atlas_rt::world::format::{get_effective_palette, get_palette, open_bytes, open_file};
use dot_vox::DotVoxData;
use glam::Vec4;

const WALL: usize = 1;
const PANE: usize = 2;
const PRODUCT: usize = 3;
const FALLBACK: usize = 4;
const ZERO: usize = 5;

fn path(name: &str) -> String {
    format!("assets/test/{name}")
}

fn load(name: &str) -> DotVoxData {
    open_file(&path(name))
}

fn bytes(name: &str) -> Vec<u8> {
    std::fs::read(path(name)).unwrap_or_else(|error| panic!("could not read {name}: {error}"))
}

fn alpha(palette: &[Vec4; 256], slot: usize) -> f32 {
    palette.get(slot).map_or(0.0, |color| color.w)
}

fn near(actual: f32, expected: f32) -> bool {
    (actual - expected).abs() < 1.0e-6
}

fn expect_error<T, E>(result: Result<T, E>, message: &str) -> E {
    match result {
        Ok(_) => panic!("{message}"),
        Err(error) => error,
    }
}

#[test]
fn matl_alpha_is_applied_to_the_one_based_palette_slot() {
    let data = load("matl-alpha.vox");
    let raw = get_palette(&data);
    let effective = get_effective_palette(&data).unwrap_or_else(|error| panic!("{error}"));

    assert!(near(alpha(&raw, WALL), 1.0));
    assert!(near(alpha(&effective, WALL), 1.0));
    assert!(near(alpha(&raw, PANE), 1.0));
    assert!(near(alpha(&effective, PANE), 0.5));
    assert!(near(alpha(&raw, PRODUCT), 128.0 / 255.0));
    assert!(near(alpha(&effective, PRODUCT), 128.0 / 255.0 * 0.5));
    assert!(near(alpha(&effective, FALLBACK), 128.0 / 255.0));
    assert!(near(alpha(&effective, ZERO), 0.0));
}

#[test]
fn invalid_matl_alpha_falls_back_to_the_palette() {
    let data =
        open_bytes(&bytes("matl-alpha-invalid.vox")).unwrap_or_else(|error| panic!("{error}"));
    let raw = get_palette(&data);
    let effective = get_effective_palette(&data).unwrap_or_else(|error| panic!("{error}"));

    assert!(near(alpha(&raw, PANE), 1.0));
    assert!(near(alpha(&effective, PANE), 1.0));
}

#[test]
fn duplicate_matl_ids_reject_the_fixture() {
    let data =
        open_bytes(&bytes("matl-alpha-duplicate.vox")).unwrap_or_else(|error| panic!("{error}"));
    let error = expect_error(get_effective_palette(&data), "duplicate ids must reject");

    assert!(error.to_string().contains("duplicate MATL id 3"));
}

#[test]
fn default_imap_is_accepted() {
    let data = load("matl-alpha-imap-default.vox");
    let effective = get_effective_palette(&data).unwrap_or_else(|error| panic!("{error}"));

    assert!(near(alpha(&effective, PANE), 0.5));
}

#[test]
fn non_default_imap_rejects_the_fixture() {
    let error = expect_error(
        open_bytes(&bytes("matl-alpha-imap.vox")),
        "non-default IMAP must reject",
    );

    assert!(error.to_string().contains("non-default IMAP"));
}

#[test]
fn malformed_imap_rejects_the_fixture() {
    let error = expect_error(
        open_bytes(&bytes("matl-alpha-imap-malformed.vox")),
        "malformed IMAP must reject",
    );

    assert!(error.to_string().contains("IMAP length"));
}
