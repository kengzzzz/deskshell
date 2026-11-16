//! The share picker's sizes. A card shorter than its content shrinks and clips its lists.

use deskshell_core::share;

pub const CARD_MAX: f32 = 2160.0;
pub const SIDE_ROOM: f32 = 200.0;
pub const TOP_ROOM: f32 = 160.0;
pub const EXTRA: f32 = 48.0;
pub const PADDING: f32 = 20.0;
pub const GAP: f32 = 20.0;
pub const SOURCE_WIDTH: f32 = 560.0;
pub const HEADER: f32 = 44.0;
pub const HEADER_RECORDING: f32 = 42.0;
pub const FOOTER: f32 = 40.0;
pub const LABEL: f32 = 16.0;
pub const COLUMN_GAP: f32 = 12.0;
pub const REGION: f32 = 84.0;
pub const SCREEN_EXTRA: f32 = 64.0;
pub const WINDOW_HEADER: f32 = 18.0;
pub const CELL_ASPECT: f32 = 0.66;
pub const CELL_MARGIN: f32 = 6.0;
pub const EMPTY: f32 = 19.0;

// Counts the gap before the screen list too.
const SOURCE_FIXED: f32 =
    LABEL + COLUMN_GAP + COLUMN_GAP + LABEL + COLUMN_GAP + REGION + COLUMN_GAP;

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Geometry {
    pub surface: (f32, f32),
    pub card: (f32, f32),
    pub row: f32,
    pub screens: Vec<f32>,
    pub list: f32,
    pub grid_width: f32,
    pub grid_height: f32,
    pub columns: usize,
    pub cell: (f32, f32),
    pub content: f32,
}

impl Geometry {
    /// `screens` are each screen's width and height in logical pixels.
    pub fn new(
        surface: (f32, f32),
        screens: impl Iterator<Item = (f32, f32)>,
        windows: usize,
        recording: bool,
    ) -> Self {
        let (width, height) = surface;
        let card_width = (width - SIDE_ROOM).clamp(0.0, CARD_MAX);
        let grid_width = (card_width - 2.0 * PADDING - SOURCE_WIDTH - GAP).max(0.0);
        let columns = share::columns(grid_width);
        let cell_width = grid_width / columns as f32;
        let cell = (cell_width, cell_width * CELL_ASPECT);
        let rows = windows.div_ceil(columns).max(1);
        let content = if windows == 0 { 0.0 } else { rows as f32 * cell.1 };

        let screens: Vec<f32> = screens
            .map(|(w, h)| screen_card(w, h))
            .collect();
        let list_content =
            screens.iter().sum::<f32>() + COLUMN_GAP * screens.len().saturating_sub(1) as f32;
        let source = list_content + SOURCE_FIXED;
        let window_column = WINDOW_HEADER + COLUMN_GAP + if windows == 0 { EMPTY } else { content };
        let header = if recording { HEADER_RECORDING } else { HEADER };
        let fixed = header + FOOTER + 2.0 * GAP;
        let implicit = fixed + source.max(window_column);

        let card_height = (height - TOP_ROOM)
            .min(implicit + EXTRA)
            .max(0.0);
        let row = (card_height - 2.0 * PADDING - fixed).max(0.0);
        Self {
            surface,
            card: (card_width, card_height),
            row,
            screens,
            list: list_content.min((row - SOURCE_FIXED).max(0.0)),
            grid_width,
            grid_height: (row - WINDOW_HEADER - COLUMN_GAP).max(0.0),
            columns,
            cell,
            content,
        }
    }

    pub fn reveal(&self, index: usize, scroll: f32) -> f32 {
        let top = (index / self.columns) as f32 * self.cell.1;
        let bottom = top + self.cell.1;
        let target = if top < scroll {
            top
        } else if bottom > scroll + self.grid_height {
            bottom - self.grid_height
        } else {
            scroll
        };
        target.clamp(0.0, self.max_scroll())
    }

    pub fn max_scroll(&self) -> f32 {
        (self.content - self.grid_height).max(0.0)
    }
}

fn screen_card(width: f32, height: f32) -> f32 {
    let preview =
        if width >= height && width > 0.0 { SOURCE_WIDTH * height / width } else { SOURCE_WIDTH };
    preview + SCREEN_EXTRA
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn portrait_screens_get_a_square_preview() {
        assert_eq!(screen_card(1080.0, 1920.0), 624.0);
        assert_eq!(screen_card(0.0, 0.0), 624.0);
        assert_eq!(screen_card(1280.0, 720.0), 379.0);
    }

    #[test]
    fn a_720p_guest_clips_the_screen_list() {
        let geometry = Geometry::new(
            (1280.0, 720.0),
            [(1280.0, 720.0)].into_iter(),
            3,
            false,
        );
        assert_eq!(geometry.card, (1080.0, 560.0), "height - 160");
        assert_eq!(geometry.grid_width, 460.0);
        assert_eq!(geometry.columns, 2);
        assert_eq!(geometry.cell.0, 230.0);
        assert_eq!(geometry.screens, [379.0]);
        assert_eq!(geometry.row, 560.0 - 40.0 - 44.0 - 40.0 - 40.0);
        assert_eq!(geometry.list, geometry.row - 164.0);
        assert_eq!(geometry.grid_height, geometry.row - 30.0);
    }

    #[test]
    fn a_1440p_screen_fits_the_content() {
        let geometry = Geometry::new(
            (2560.0, 1440.0),
            [(2560.0, 1440.0)].into_iter(),
            4,
            false,
        );
        assert_eq!(geometry.card.0, 2160.0);
        assert_eq!(geometry.columns, 3);
        let cell = 1540.0 / 3.0 * 0.66;
        assert!((geometry.content - 2.0 * cell).abs() < 0.01);
        let implicit = 44.0 + 40.0 + 40.0 + (30.0 + geometry.content);
        assert!((geometry.card.1 - (implicit + 48.0)).abs() < 0.01);
        assert_eq!(geometry.list, 379.0, "the whole list");
        assert_eq!(geometry.max_scroll(), 0.0);
    }

    #[test]
    fn no_windows_and_two_screens() {
        let screens = [(2560.0, 1440.0), (1920.0, 1200.0)];
        let geometry = Geometry::new((2560.0, 1440.0), screens.into_iter(), 0, true);
        assert_eq!(geometry.screens, [379.0, 414.0]);
        assert_eq!(geometry.content, 0.0);
        let implicit = 42.0 + 40.0 + 40.0 + 379.0 + 12.0 + 414.0 + 164.0;
        assert_eq!(geometry.card.1, implicit + 48.0);
    }

    #[test]
    fn reveal_keeps_the_selected_row_in_view() {
        let geometry = Geometry::new(
            (1280.0, 720.0),
            [(1280.0, 720.0)].into_iter(),
            9,
            false,
        );
        let cell = geometry.cell.1;
        let view = geometry.grid_height;
        assert!(geometry.max_scroll() > 0.0);
        assert_eq!(geometry.reveal(0, 0.0), 0.0);
        assert_eq!(geometry.reveal(1, 0.0), 0.0, "same row");
        assert!(
            (geometry.reveal(4, 0.0) - (3.0 * cell - view)).abs() < 0.01,
            "row 2 at the bottom"
        );
        assert_eq!(geometry.reveal(0, 200.0), 0.0, "back up to row 0");
        assert_eq!(geometry.reveal(8, 0.0), geometry.max_scroll());
    }
}
