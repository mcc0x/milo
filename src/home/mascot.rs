//! Color-cell renditions of `docs/img/milo.png`, cropped to its transparent silhouette.

use crate::screen::{Screen, Style};

// The ASCII pixels below index this xterm palette: A–O are full cells,
// a–o are upper half cells, 0–> are lower half cells, and dots are transparent.
const PALETTE: [u8; 15] = [17, 18, 19, 25, 26, 27, 32, 33, 39, 75, 81, 147, 189, 255, 231];

const LARGE: &[&[u8]] = &[
    b".......7JJ99..........9999......",
    b"......HHHHHJJ9.....77HJHFH9.....",
    b".....FHHFFFFHJJ9999HJJHFFFH.....",
    b"....FFHHFFFHHJJJJJJJKKKJJHH9....",
    b"...5FFHFFHHJJJJJJJJJKKKKKKJJ....",
    b"...FFFHHHHHJJJJJCCJJJKMMKCCJ9...",
    b"...FFFFHHHHJJJJJBBBCJMNOOMBBK9..",
    b"..FFFFFHHHHHJJJJJBBJMNMNCNCNOM..",
    b".FFFFFHMMMMMMJJJJMMNNNBBCCNONNN.",
    b".FFFFFFMHHMMMMMNNNNNNNNNNNNNNNM.",
    b".hFFFFFFMMMHMMMNNNNNNNNNNNMMMM..",
    b"...FFFFFFFHMMMMMMMMMMMMMMMMMm...",
    b"....hfFFFFLMMMMMMMMMMMMMMmm.....",
    b".......FFFFHJLLMMMMMMMMM........",
    b"......FFFFFHHHHLMMMMMNNNN.......",
    b".....ffFFFFHHHHJMMMMMMMMm.......",
];

const MEDIUM: &[&[u8]] = &[
    b"....77J99.....7999....",
    b"...5HHFFHJ999HHHFF7...",
    b"...FHFFHHJJJJJKKJHH...",
    b"..FFHHHHJJJJJJKKKCJ9..",
    b"..FFFHHHJJJBBCKNOMBJ9.",
    b".FFFFHMMMJJJCMNCCCNON.",
    b".FFFHMJMMMMNNNNNNNNNM.",
    b".fFFFHJHMMMMMMMMMMMMm.",
    b"...fFFFLMMMMMMMMMMm...",
    b"....5FFFHJLMMMMMM.....",
    b"....FFFHHHJMMMMMM.....",
];

const SMALL: &[&[u8]] = &[
    b"..7HH9...9H7..", b"..HHFHJJJJJH..", b".FFHHJJCCKMC9.", b"5FFMMMJCCMCMN=", b"fFFHJLMNNNNNMm", b"..fFJLMMMMMm..",
    b"..5FFHJMMMM...",
];

const TINY: &[&[u8]] = &[b".7H99J7.", b"5FHJCMC<", b"fFJLMMMn", b".FHJMM.."];

#[derive(Clone, Copy)]
pub(super) struct Sprite(&'static [&'static [u8]]);

impl Sprite {
    pub(super) fn fit(width: usize, height: usize) -> Option<Self> {
        [LARGE, MEDIUM, SMALL, TINY]
            .into_iter()
            .map(Self)
            .find(|sprite| sprite.width() <= width && sprite.height() <= height)
    }

    pub(super) fn width(self) -> usize {
        self.0[0].len()
    }

    pub(super) fn height(self) -> usize {
        self.0.len()
    }

    pub(super) fn draw(self, s: &mut Screen, x: usize, y: usize) {
        for (dy, row) in self.0.iter().enumerate() {
            for (dx, &cell) in row.iter().enumerate() {
                let (index, pixels) = match cell {
                    b'A'..=b'C' => (cell - b'A', "▓"),
                    b'D'..=b'O' => (cell - b'A', "█"),
                    b'a'..=b'o' => (cell - b'a', "▀"),
                    b'0'..=b'>' => (cell - b'0', "▄"),
                    _ => continue,
                };
                s.put(x + dx, y + dy, 1, pixels, Style::fg(PALETTE[usize::from(index)]));
            }
        }
    }
}
