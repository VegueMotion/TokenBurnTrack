mod bar;
pub mod format;
mod heatmap;
mod line;
mod paint;
pub mod scale;
mod sparkline;
pub mod stack;
pub mod time;
pub mod timeline;

use gpui::{Hsla, IntoElement, ParentElement, SharedString, Styled, div, px};
use gpui_component::h_flex;

pub use bar::{BarChart, bar_chart};
pub use format::ValueKind;
pub use heatmap::{HeatMetric, HeatmapChart, heatmap_chart};
pub use line::{LineChart, LineMode, line_chart};
pub use paint::{ChartTheme, Hover};
pub use sparkline::sparkline;

#[derive(Clone, Debug)]
pub struct Series {
    pub name: SharedString,
    pub color: Hsla,
    pub values: Vec<f64>,
}

pub fn legend(
    items: impl IntoIterator<Item = (SharedString, Hsla)>,
    text: Hsla,
) -> impl IntoElement {
    h_flex()
        .flex_wrap()
        .gap_x_4()
        .gap_y_1()
        .text_xs()
        .text_color(text)
        .children(items.into_iter().map(|(name, color)| {
            h_flex()
                .gap_1p5()
                .items_center()
                .child(div().size(px(9.)).rounded(px(2.)).bg(color))
                .child(name)
        }))
}
