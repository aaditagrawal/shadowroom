use super::*;

pub(super) const WIDTH: f32 = 12.;

#[derive(Clone, Copy, Debug)]
struct Geometry {
    thumb: f32,
    top: f32,
    travel: f32,
    maximum: f32,
}
impl Geometry {
    fn new(height: f32, maximum: f32, offset: f32) -> Self {
        let maximum = maximum.max(0.);
        let thumb = (height * height / (height + maximum).max(1.))
            .max(24.)
            .min(height.max(0.));
        let travel = (height - thumb).max(0.);
        Self {
            thumb,
            top: if maximum > 0. {
                (-offset / maximum).clamp(0., 1.) * travel
            } else {
                0.
            },
            travel,
            maximum,
        }
    }
    fn offset(self, top: f32) -> f32 {
        if self.travel > 0. {
            -self.maximum * (top / self.travel).clamp(0., 1.)
        } else {
            0.
        }
    }
}

pub(super) struct Drag {
    handle: ScrollHandle,
    track_top: f32,
    grab: f32,
    geometry: Geometry,
}
impl Workspace {
    pub(super) fn move_scrollbar(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        if let Some(drag) = &self.scroll_drag {
            if !event.dragging() {
                self.scroll_drag = None;
                return;
            }
            drag.handle.set_offset(point(
                px(0.),
                px(drag
                    .geometry
                    .offset(f32::from(event.position.y) - drag.track_top - drag.grab)),
            ));
            cx.notify();
        }
    }

    pub(super) fn scrollable_list(
        &self,
        id: &'static str,
        handle: &UniformListScrollHandle,
        content: AnyElement,
        cx: &Context<Self>,
    ) -> AnyElement {
        let base = handle.0.borrow().base_handle.clone();
        let paint_handle = base.clone();
        let geometry = std::rc::Rc::new(std::cell::Cell::new((0., Geometry::new(0., 0., 0.))));
        let painted = geometry.clone();
        let muted = self.icon_muted();
        let rail = self.border();
        let scrollbar = div()
            .id(id)
            .w(px(WIDTH))
            .h_full()
            .flex_shrink_0()
            .cursor_pointer()
            .child(
                canvas(
                    move |bounds, _, _| {
                        let g = Geometry::new(
                            f32::from(bounds.size.height),
                            f32::from(paint_handle.max_offset().height),
                            f32::from(paint_handle.offset().y),
                        );
                        painted.set((f32::from(bounds.top()), g));
                        g
                    },
                    move |bounds, g, window, _| {
                        if g.maximum > 0. {
                            window.paint_quad(fill(bounds, rail.opacity(0.3)));
                            window.paint_quad(fill(
                                Bounds::new(
                                    point(bounds.left() + px(3.), bounds.top() + px(g.top)),
                                    size(px(WIDTH - 6.), px(g.thumb)),
                                ),
                                muted,
                            ));
                        }
                    },
                )
                .size_full(),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                    let (track_top, g) = geometry.get();
                    if g.maximum <= 0. {
                        return;
                    }
                    let y = f32::from(event.position.y) - track_top;
                    let grab = if y >= g.top && y <= g.top + g.thumb {
                        y - g.top
                    } else {
                        g.thumb / 2.
                    };
                    base.set_offset(point(px(0.), px(g.offset(y - grab))));
                    this.scroll_drag = Some(Drag {
                        handle: base.clone(),
                        track_top,
                        grab,
                        geometry: g,
                    });
                    cx.stop_propagation();
                    cx.notify();
                }),
            );
        div()
            .flex()
            .size_full()
            .min_h(px(0.))
            .min_w(px(0.))
            .child(
                div()
                    .flex_1()
                    .min_h(px(0.))
                    .min_w(px(0.))
                    .h_full()
                    .child(content),
            )
            .child(scrollbar)
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::Geometry;

    #[test]
    fn scrollbar_reaches_both_ends_and_keeps_a_grabbable_thumb() {
        for maximum in [0., 100., 100_000.] {
            let g = Geometry::new(600., maximum, -maximum / 2.);
            assert!((24. ..=600.).contains(&g.thumb));
            assert_eq!(g.offset(-20.), 0.);
            assert_eq!(g.offset(g.travel + 20.), -maximum);
            if maximum > 0. {
                assert!((g.top - g.travel / 2.).abs() < 0.01);
            }
        }
    }
}
