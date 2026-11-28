use gpui::{
    AnyElement, App, Hsla, IntoElement, ParentElement, SharedString, Styled, div, prelude::*, px,
};
use gpui_component::{ActiveTheme, StyledExt, h_flex, scroll::ScrollableElement, v_flex};

pub fn card(title: impl Into<SharedString>, cx: &App) -> gpui::Div {
    let t = cx.theme();
    v_flex()
        .gap_3()
        .p_4()
        .rounded_lg()
        .border_1()
        .border_color(t.border)
        .bg(t.secondary.opacity(0.35))
        .child(
            div()
                .text_sm()
                .font_semibold()
                .text_color(t.foreground)
                .child(title.into()),
        )
}

pub fn muted_text(text: impl Into<SharedString>, cx: &App) -> impl IntoElement {
    div()
        .text_sm()
        .text_color(cx.theme().muted_foreground)
        .child(text.into())
}

pub fn status_message(text: impl Into<SharedString>, cx: &App) -> AnyElement {
    div()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .child(muted_text(text, cx))
        .into_any_element()
}

pub fn page_scroll(id: &'static str, content: impl IntoElement) -> impl IntoElement {
    div()
        .size_full()
        .overflow_y_scrollbar()
        .id(id)
        .child(content)
}

pub fn swatch(color: Hsla) -> impl IntoElement {
    div().flex_none().size(px(9.)).rounded(px(2.)).bg(color)
}

#[derive(Clone, Copy)]
pub struct Col {
    pub title: &'static str,
    pub width: Option<f32>,
    pub right: bool,
}

impl Col {
    pub const fn flex(title: &'static str) -> Col {
        Col {
            title,
            width: None,
            right: false,
        }
    }

    pub const fn fixed(title: &'static str, width: f32) -> Col {
        Col {
            title,
            width: Some(width),
            right: false,
        }
    }

    pub const fn num(title: &'static str, width: f32) -> Col {
        Col {
            title,
            width: Some(width),
            right: true,
        }
    }

    pub fn cell(&self, content: impl IntoElement) -> gpui::Div {
        let d = div().overflow_hidden().whitespace_nowrap().text_ellipsis();
        let d = match self.width {
            Some(w) => d.w(px(w)).flex_none(),
            None => d.flex_1().min_w(px(80.)),
        };
        let d = if self.right {
            d.flex().justify_end()
        } else {
            d
        };
        d.child(content)
    }
}

pub fn table(cols: &[Col], rows: Vec<Vec<AnyElement>>, cx: &App) -> impl IntoElement {
    let t = cx.theme();
    let (muted, border, even) = (t.muted_foreground, t.border, t.foreground.opacity(0.025));
    v_flex()
        .w_full()
        .text_sm()
        .child(
            h_flex()
                .gap_3()
                .px_2()
                .py_1p5()
                .border_b_1()
                .border_color(border)
                .text_xs()
                .text_color(muted)
                .children(cols.iter().map(|c| c.cell(c.title))),
        )
        .children(rows.into_iter().enumerate().map(|(i, cells)| {
            h_flex()
                .gap_3()
                .px_2()
                .py_1p5()
                .when(i % 2 == 1, |d| d.bg(even))
                .children(cols.iter().zip(cells).map(|(c, cell)| c.cell(cell)))
        }))
}

pub fn bar_cell(fraction: f64, color: Hsla, track: Hsla) -> impl IntoElement {
    div().h(px(6.)).w_full().rounded(px(3.)).bg(track).child(
        div()
            .h_full()
            .rounded(px(3.))
            .bg(color)
            .w(gpui::relative(fraction.clamp(0.0, 1.0) as f32)),
    )
}
