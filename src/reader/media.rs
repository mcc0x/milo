//! Terminal media stays inside the Reader's content rectangle, including under a tab strip.

use super::video::Video;
use crate::help::{self, HelpRow};
use crate::keys::Key;
use crate::pane::After;
use crate::picture::Picture;
use crate::screen::{Screen, Style};

pub(super) fn picture(s: &mut Screen, rows: usize, picture: &Picture) -> Option<&'static str> {
    let room = s.w.saturating_sub(2);
    if room == 0 || rows == 0 {
        return None;
    }
    if s.pictures() == Some(true) {
        let size = picture.fit(s.cell_size(), room, rows);
        let origin = (1 + (room - size.0) / 2, 1 + (rows - size.1) / 2);
        s.show_picture(picture, origin.0, origin.1, size.0, size.1);
    } else if let Some(rgb) = picture.raster() {
        fitted_cells(s, rows, rgb);
    } else if s.pictures() == Some(false) {
        return Some(NO_PICTURES);
    }
    None
}

#[cfg(not(windows))]
const NO_PICTURES: &str = "Kitty graphics and a cell preview are unavailable here.";
#[cfg(windows)]
const NO_PICTURES: &str = "Pictures need Kitty graphics here; a cell preview is not supported on Windows yet.";

pub(super) fn movie<'a>(s: &mut Screen, rows: usize, video: &'a Video) -> Option<&'a str> {
    if let Some(why) = video.error() {
        return Some(why);
    }
    let Some(frame) = video.frame() else { return Some("Reading video frame…") };
    let room = s.w.saturating_sub(2);
    if room == 0 || rows == 0 {
        return None;
    }
    fitted_cells(s, rows, (&frame.rgb, frame.width, frame.height));
    None
}

fn fitted_cells(s: &mut Screen, rows: usize, rgb: (&[u8], usize, usize)) {
    let room = s.w.saturating_sub(2);
    let (cw, ch) = s.cell_size();
    let wide = rgb.1 * usize::from(ch);
    let tall = rgb.2 * usize::from(cw);
    // A cell carries two samples, not a full bitmap tile. Use the available
    // samples for small icons rather than reducing a 16×16 icon to 2×1 cells.
    let cols = room.min(rgb.1).min((rows * wide / tall).max(1));
    let height = rows.min((cols * tall).div_ceil(wide).max(1));
    let origin = (1 + (room - cols) / 2, 1 + (rows - height) / 2);
    cells(s, origin, (cols, height), rgb);
}

/// Each cell takes two samples, drawn as two palette colours or one shade.
fn cells(s: &mut Screen, (x, y): (usize, usize), (cols, rows): (usize, usize), rgb: (&[u8], usize, usize)) {
    let (pixels, width, height) = rgb;
    let color = s.raster_colors();
    for row in 0..rows {
        let sample_row = |half: usize| ((4 * row + half) * height / (4 * rows)).min(height - 1) * width;
        let (upper, lower) = (sample_row(1), sample_row(3));
        for col in 0..cols {
            let sx = (2 * col + 1) * width / (2 * cols);
            let sample = |start: usize| {
                let i: usize = 3 * (start + sx);
                (pixels[i], pixels[i + 1], pixels[i + 2])
            };
            let (top, bottom) = (sample(upper), sample(lower));
            if color {
                let style = Style::pixels(palette(top.0, top.1, top.2), palette(bottom.0, bottom.1, bottom.2));
                s.put(x + col, y + row, 1, "▀", style);
            } else {
                let value = |(r, g, b)| usize::from(r) * 3 + usize::from(g) * 6 + usize::from(b);
                let level = (value(top) + value(bottom) + 637) / 1275;
                s.put(x + col, y + row, 1, [" ", "░", "▒", "▓", "█"][level], Style::PLAIN);
            }
        }
    }
}

fn palette(r: u8, g: u8, b: u8) -> u8 {
    if r == g && g == b && (4..244).contains(&g) {
        return 232 + (g - 4) / 10;
    }
    // Nearest channel in xterm's 0, 95, 135, 175, 215, 255 color cube.
    let level = |c: u8| {
        if c < 48 {
            0
        } else if c < 115 {
            1
        } else {
            2 + (c - 115) / 40
        }
    };
    16 + 36 * level(r) + 6 * level(g) + level(b)
}

pub(super) fn input(video: &mut Video, key: Key) -> Option<After> {
    match key {
        Key::Char(' ') => video.toggle(),
        Key::Left | Key::Char('h') => video.seek(-5),
        Key::Right | Key::Char('l') => video.seek(5),
        Key::Focus(false) => video.pause(),
        _ => return None,
    }
    Some(After::Stay)
}

pub(super) fn status<'a>(video: &Video, buffer: &'a mut [u8; 64]) -> &'a str {
    use std::io::Write as _;

    if video.duration().is_zero() {
        return "Reading video…";
    }
    let (at, end) = (video.position().as_secs(), video.duration().as_secs());
    let state = if video.is_playing() { "Playing" } else { "Paused" };
    let size = buffer.len();
    let mut out = &mut buffer[..];
    if write!(out, "{state} {}:{:02} / {}:{:02} · muted", at / 60, at % 60, end / 60, end % 60).is_err() {
        return state;
    }
    let used = size - out.len();
    std::str::from_utf8(&buffer[..used]).expect("video status is UTF-8")
}

pub(super) fn help_rows(video: &Video) -> Vec<HelpRow> {
    if video.error().is_some() {
        return vec![help::heading("Video unavailable")];
    }
    vec![help::heading("Video · muted"), help::key("Space", "play/pause"), help::key("← →  ", "seek 5 seconds")]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::term::{Capabilities, Colors};

    #[test]
    fn a_low_colour_terminal_draws_rgb_samples_as_a_shade() {
        let mut s = Screen::for_terminal(Capabilities { colors: Colors::Ansi, graphics: false });
        s.resize(8, 5);
        cells(&mut s, (1, 1), (1, 1), (&[0, 0, 0, 255, 255, 255], 1, 2));
        let out = String::from_utf8(s.emitted()).unwrap();
        assert!(out.contains("\x1b[2;2H\x1b[0m▒"), "{out:?}");
    }

    #[test]
    fn a_red_over_blue_cell_is_positioned_below_the_strip_and_erased_on_close() {
        let mut s = Screen::sized(8, 5);
        s.emitted();
        s.below(1, |s| cells(s, (3, 1), (1, 1), (&[255, 0, 0, 0, 0, 255], 1, 2)));
        let out = String::from_utf8(s.emitted()).unwrap();
        assert!(out.contains("\x1b[3;4H\x1b[0;38;5;196;48;5;21m▀"), "{out:?}");
        s.clear();
        assert!(String::from_utf8(s.emitted()).unwrap().contains("\x1b[3;4H\x1b[0m "));
    }
}
