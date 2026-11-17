use egui::{Pos2, pos2};

fn dot(c: Pos2) -> Vec<Pos2> {
    vec![c + egui::vec2(-0.75, -0.75), c + egui::vec2(0.75, -0.75), c + egui::vec2(0.75, 0.75), c + egui::vec2(-0.75, 0.75)]
}

/// Closed outline of an I-beam on its side (stem across, serifs at the ends), centred on the hotspot.
fn sideways_i_beam() -> Vec<Pos2> {
    [(-6.0, -2.5), (-5.0, -2.5), (-5.0, -0.5), (5.0, -0.5), (5.0, -2.5), (6.0, -2.5), (6.0, 2.5), (5.0, 2.5), (5.0, 0.5), (-5.0, 0.5), (-5.0, 2.5), (-6.0, 2.5)]
        .into_iter()
        .map(|(x, y)| pos2(x, y))
        .collect()
}

/// The dotted box of the Vertical Type tool's pointer: its top-left corner 9 points left of and
/// 4 above the hotspot (the reference's hotspot is (9,4) in its cursor image), 18 wide and 16
/// tall, so the column the text runs down sits below the sideways I-beam.
const VERTICAL_TYPE_BOX: [f32; 4] = [-9.0, -4.0, 9.0, 12.0];

/// The Vertical Type tool's pointer over the canvas (#12063): the sideways I-beam inside a
/// dotted box, the hotspot in the middle of the I-beam's stem.
pub(crate) fn vertical_type_rings() -> Vec<Vec<Pos2>> {
    let [x0, y0, x1, y1] = VERTICAL_TYPE_BOX;
    let mut rings = vec![sideways_i_beam()];
    // Four dots along each edge, corners shared, spaced so none touches the I-beam's serifs.
    rings.extend((0..4).map(|i| x0 + (x1 - x0) * i as f32 / 3.0).flat_map(|x| [dot(pos2(x, y0)), dot(pos2(x, y1))]));
    rings.extend((1..3).map(|i| y0 + (y1 - y0) * i as f32 / 3.0).flat_map(|y| [dot(pos2(x0, y)), dot(pos2(x1, y))]));
    rings
}
