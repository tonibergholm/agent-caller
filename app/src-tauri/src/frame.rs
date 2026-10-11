//! Panel frame (position and size) persistence and placement. Everything here
//! is pure or plain file I/O so it can be tested without a window.
//!
//! Units are logical points in the global screen space Tauri reports on
//! macOS (origin at the top left of the primary display).
use std::fs;
use std::io::Write;
use std::path::Path;

use serde::{Deserialize, Serialize};

pub const DEFAULT_SIZE: (f64, f64) = (560.0, 720.0);
pub const MIN_SIZE: (f64, f64) = (380.0, 320.0);

/// A saved frame whose overlap with every screen is below this share of its
/// own area counts as off-screen and is moved to the fallback screen.
const MIN_VISIBLE_SHARE: f64 = 0.25;

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
pub struct Frame {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl Frame {
    fn sane(&self) -> bool {
        [self.x, self.y, self.w, self.h].iter().all(|v| v.is_finite()) && self.w > 0.0 && self.h > 0.0
    }

    fn overlap_area(&self, other: &Frame) -> f64 {
        let w = (self.x + self.w).min(other.x + other.w) - self.x.max(other.x);
        let h = (self.y + self.h).min(other.y + other.h) - self.y.max(other.y);
        if w > 0.0 && h > 0.0 { w * h } else { 0.0 }
    }
}

/// Fits `frame` inside `screen`: size within [MIN_SIZE, screen] (the screen
/// wins when it is smaller than the minimum), position kept on the screen.
fn clamp_into(frame: Frame, screen: &Frame) -> Frame {
    let w = frame.w.max(MIN_SIZE.0).min(screen.w);
    let h = frame.h.max(MIN_SIZE.1).min(screen.h);
    Frame {
        x: frame.x.clamp(screen.x, screen.x + screen.w - w),
        y: frame.y.clamp(screen.y, screen.y + screen.h - h),
        w,
        h,
    }
}

fn centered(w: f64, h: f64, screen: &Frame) -> Frame {
    clamp_into(
        Frame {
            x: screen.x + (screen.w - w) / 2.0,
            y: screen.y + (screen.h - h) / 2.0,
            w,
            h,
        },
        screen,
    )
}

/// Decides where the panel goes.
///
/// - `saved` is the remembered or current frame, if any.
/// - `screens` are the usable areas (menu bar and Dock excluded) of every display.
/// - `fallback` indexes the screen to use when nothing was saved or the saved
///   frame is off every screen (the one under the cursor, else the primary).
///
/// A saved frame that still overlaps a screen stays on that screen, clamped
/// into it. Otherwise the saved size is kept and the panel is centered on the
/// fallback screen. With no saved frame the default size is centered there.
pub fn resolve(saved: Option<Frame>, screens: &[Frame], fallback: usize) -> Option<Frame> {
    let fallback_screen = screens.get(fallback).or_else(|| screens.first())?;
    let Some(saved) = saved.filter(Frame::sane) else {
        return Some(centered(DEFAULT_SIZE.0, DEFAULT_SIZE.1, fallback_screen));
    };
    let best = screens
        .iter()
        .map(|s| (s, saved.overlap_area(s)))
        .max_by(|a, b| a.1.total_cmp(&b.1))
        .filter(|(_, area)| *area >= MIN_VISIBLE_SHARE * saved.w * saved.h);
    Some(match best {
        Some((screen, _)) => clamp_into(saved, screen),
        None => centered(saved.w, saved.h, fallback_screen),
    })
}

/// A missing, unreadable or malformed file is "nothing saved".
pub fn load(path: &Path) -> Option<Frame> {
    let raw = fs::read_to_string(path).ok()?;
    serde_json::from_str::<Frame>(&raw).ok().filter(Frame::sane)
}

/// Atomic write (temp file, then rename) so a crash never leaves half a file.
pub fn save(path: &Path, frame: &Frame) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    let mut f = fs::File::create(&tmp)?;
    f.write_all(serde_json::to_string(frame).expect("serializable").as_bytes())?;
    f.sync_all()?;
    fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn f(x: f64, y: f64, w: f64, h: f64) -> Frame {
        Frame { x, y, w, h }
    }
    const MAIN: Frame = f(0.0, 25.0, 1440.0, 800.0);
    const RIGHT: Frame = f(1440.0, 0.0, 1920.0, 1080.0);

    #[test]
    fn nothing_saved_centers_the_default_size_on_the_fallback() {
        let r = resolve(None, &[MAIN, RIGHT], 1).unwrap();
        assert_eq!((r.w, r.h), DEFAULT_SIZE);
        assert_eq!(r.x, 1440.0 + (1920.0 - DEFAULT_SIZE.0) / 2.0);
        assert_eq!(r.y, (1080.0 - DEFAULT_SIZE.1) / 2.0);
    }

    #[test]
    fn a_visible_frame_is_kept_exactly() {
        let saved = f(100.0, 100.0, 500.0, 600.0);
        assert_eq!(resolve(Some(saved), &[MAIN, RIGHT], 0), Some(saved));
    }

    #[test]
    fn a_frame_on_the_second_screen_stays_there_even_if_the_fallback_is_the_first() {
        let saved = f(1600.0, 100.0, 500.0, 600.0);
        assert_eq!(resolve(Some(saved), &[MAIN, RIGHT], 0), Some(saved));
    }

    #[test]
    fn a_removed_display_moves_the_frame_to_the_fallback_keeping_its_size() {
        let saved = f(1600.0, 100.0, 500.0, 600.0);
        let r = resolve(Some(saved), &[MAIN], 0).unwrap();
        assert_eq!((r.w, r.h), (500.0, 600.0));
        assert_eq!(r.x, (1440.0 - 500.0) / 2.0);
        assert_eq!(r.y, 25.0 + (800.0 - 600.0) / 2.0);
    }

    #[test]
    fn a_frame_hanging_off_the_edge_is_pulled_back_on_screen() {
        let saved = f(1300.0, 700.0, 500.0, 600.0); // 140 x 100 visible: under the share
        let r = resolve(Some(saved), &[MAIN], 0).unwrap();
        assert!(r.x >= 0.0 && r.x + r.w <= 1440.0 && r.y >= 25.0 && r.y + r.h <= 825.0);
        let mostly = f(1100.0, 300.0, 500.0, 400.0); // 340 x 400 visible: kept, clamped
        let r = resolve(Some(mostly), &[MAIN], 0).unwrap();
        assert_eq!((r.x, r.y, r.w, r.h), (940.0, 300.0, 500.0, 400.0));
    }

    #[test]
    fn a_frame_larger_than_the_screen_shrinks_to_it() {
        let r = resolve(Some(f(0.0, 0.0, 5000.0, 4000.0)), &[MAIN], 0).unwrap();
        assert_eq!(r, MAIN);
    }

    #[test]
    fn a_frame_below_the_minimum_grows_to_it() {
        let r = resolve(Some(f(100.0, 100.0, 10.0, 10.0)), &[MAIN], 0).unwrap();
        assert_eq!((r.w, r.h), MIN_SIZE);
    }

    #[test]
    fn a_screen_smaller_than_the_minimum_wins() {
        let tiny = f(0.0, 0.0, 300.0, 200.0);
        let r = resolve(None, &[tiny], 0).unwrap();
        assert_eq!(r, tiny);
    }

    #[test]
    fn garbage_frames_are_treated_as_unsaved() {
        for bad in [f(f64::NAN, 0.0, 400.0, 400.0), f(0.0, 0.0, 0.0, 400.0), f(0.0, 0.0, -5.0, 400.0)] {
            let r = resolve(Some(bad), &[MAIN], 0).unwrap();
            assert_eq!((r.w, r.h), DEFAULT_SIZE);
        }
    }

    #[test]
    fn no_screens_gives_no_placement_and_a_bad_fallback_index_uses_the_first() {
        assert_eq!(resolve(None, &[], 0), None);
        let r = resolve(None, &[MAIN], 9).unwrap();
        assert_eq!((r.w, r.h), DEFAULT_SIZE);
    }

    #[test]
    fn the_file_round_trips_and_bad_files_load_as_nothing() {
        let dir = std::env::temp_dir().join(format!("acd-frame-{}", std::process::id()));
        let path = dir.join("nested").join("panel-frame.json");
        assert_eq!(load(&path), None);
        let frame = f(12.5, 30.0, 600.0, 700.0);
        save(&path, &frame).unwrap();
        assert_eq!(load(&path), Some(frame));
        save(&path, &f(1.0, 2.0, 400.0, 400.0)).unwrap();
        assert_eq!(load(&path), Some(f(1.0, 2.0, 400.0, 400.0)));
        fs::write(&path, "{not json").unwrap();
        assert_eq!(load(&path), None);
        fs::write(&path, r#"{"x":0,"y":0,"w":0,"h":10}"#).unwrap();
        assert_eq!(load(&path), None);
        let _ = fs::remove_dir_all(&dir);
    }
}
