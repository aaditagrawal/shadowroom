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
    /// Prepares the previous and next photos' fit previews (with focus
    /// peaking or clipping when on) and paints them invisibly, so the atlas
    /// already holds them when navigation arrives. Zoomed views are not
    /// painted: full-resolution textures are too large to keep speculatively.
    pub(super) fn neighbour_textures(
        &mut self,
        cx: &mut gpui::Context<Self>,
    ) -> Option<AnyElement> {
        if !matches!(self.mode, ViewMode::Loupe | ViewMode::Compare) {
            return None;
        }
        if self.zoom {
            // Peaking and clipping copies take a full-resolution pass each, so
            // without this the overlay pops in after every step (REV-29).
            // Prepare the next frame's copy only, without painting it.
            if self.display_effect(PreviewKind::Full) != 0 {
                if let Some(&index) = self
                    .selected
                    .checked_add_signed(self.navigation_direction)
                    .and_then(|position| self.filtered.get(position))
                {
                    let _ = self.texture(index, PreviewKind::Full, cx);
                }
            }
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
