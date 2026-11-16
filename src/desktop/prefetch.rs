//! Display textures for the frames next to the loupe photo.
//!
//! A decoded preview reaches the screen in two steps: a background pass turns
//! it into a GPUI `RenderImage`, then the first `paint_image` uploads it to the
//! GPU atlas. Read-ahead already decodes the neighbours, but without this the
//! two steps ran only after navigation, so each step showed the thumbnail
//! first and the sharp preview a frame or more later, with the upload in that
//! later frame (docs/PERFORMANCE.md, "Human interface responsiveness").
use super::{PreviewKind, ViewMode, Workspace};
use gpui::{AnyElement, IntoElement, ParentElement, Styled, div, img, px};

impl Workspace {
    /// Prepares the previous and next photos' fit previews and paints them
    /// invisibly, so the atlas already holds them when navigation arrives.
    /// Zoomed views are skipped: full-resolution textures are too large to
    /// keep speculatively.
    pub(super) fn neighbour_textures(
        &mut self,
        cx: &mut gpui::Context<Self>,
    ) -> Option<AnyElement> {
        if self.zoom || !matches!(self.mode, ViewMode::Loupe | ViewMode::Compare) {
            return None;
        }
        let mut images = Vec::new();
        for distance in [self.navigation_direction, -self.navigation_direction] {
            let Some(&index) = self
                .selected
                .checked_add_signed(distance)
                .and_then(|position| self.filtered.get(position))
            else {
                continue;
            };
            if let Some((image, ..)) = self
                .texture(index, PreviewKind::Fit, cx)
                .filter(|texture| texture.4 != PreviewKind::Thumbnail)
            {
                images.push(img(image).size_full());
            }
        }
        (!images.is_empty()).then(|| {
            div()
                .absolute()
                .size(px(1.))
                .opacity(0.)
                .children(images)
                .into_any_element()
        })
    }
}
