//! The Magic Wand's Anti-alias smooths stair-steps: next to a diagonal or curved edge, the pixel
//! just outside the region gets a coverage from the shape of the edge, whatever the colour next
//! to it (#13884). The fringe is the larger of this stair coverage and the colour fade. The
//! region itself stays fully selected. Area 64 x 64: a red 45 degree triangle (pixels with
//! y >= x in the box 10,10–40,40), a slope-1/4 edge (red below y = 20 + x/4, x 8–48, down to
//! y 50), a plain rectangle and an L shape, each on a transparent layer unless noted.

use photocraft_geom::Rect;

use super::{Region, magic_wand, wand_region};

const AREA: Rect = Rect { x0: 0, y0: 0, x1: 64, y1: 64 };
const W: usize = 64;
const H: usize = 64;
const RED: [u8; 4] = [220, 30, 30, 255];

fn blank(bg: [u8; 4]) -> Vec<[u8; 4]> {
    vec![bg; W * H]
}

/// The 45 degree triangle on `bg`.
fn triangle(bg: [u8; 4]) -> Vec<[u8; 4]> {
    let mut img = blank(bg);
    for y in 10..40 {
        for x in 10..40 {
            if y >= x {
                img[y * W + x] = RED;
            }
        }
    }
    img
}

/// Red where y >= 20 + x / 4 (x 8–48, y below 50), on a transparent layer.
fn slope() -> Vec<[u8; 4]> {
    let mut img = blank([0; 4]);
    for y in 0..50 {
        for x in 8..48 {
            if y >= 20 + x / 4 {
                img[y * W + x] = RED;
            }
        }
    }
    img
}

fn fill(img: &mut [[u8; 4]], x0: usize, y0: usize, x1: usize, y1: usize) {
    for y in y0..y1 {
        for x in x0..x1 {
            img[y * W + x] = RED;
        }
    }
}

fn coverage(r: &Region, x: i32, y: i32) -> u8 {
    (r.at(x, y) * 255.0).round() as u8
}

/// Pixels of the area that are neither fully selected nor unselected.
fn partial(r: &Region) -> usize {
    (0..H as i32).flat_map(|y| (0..W as i32).map(move |x| (x, y))).filter(|&(x, y)| !matches!(coverage(r, x, y), 0 | 255)).count()
}

#[test]
fn a_45_degree_staircase_on_a_transparent_layer_gets_half_coverage() {
    let img = triangle([0; 4]);
    let r = wand_region(&img, AREA, (30, 35), 32.0, true, true).unwrap();
    for y in 10..40 {
        for x in 10..40 {
            if y >= x {
                assert_eq!(coverage(&r, x as i32, y as i32), 255, "region pixel {x},{y}");
            }
        }
    }
    for x in 11..=38 {
        assert_eq!(coverage(&r, x, x - 1), 128, "step pixel {x},{}", x - 1);
        assert_eq!(coverage(&r, x, x - 3), 0, "two steps away {x},{}", x - 3);
    }
}

#[test]
fn a_45_degree_staircase_next_to_a_very_different_colour_is_softened() {
    // Red on opaque black, clicking the black: the colour fringe of the red is small (about
    // 33), but the staircase is smoothed all the same.
    let img = triangle([0, 0, 0, 255]);
    let r = wand_region(&img, AREA, (60, 5), 32.0, true, true).unwrap();
    for x in 12..=36 {
        assert!(coverage(&r, x, x) >= 128, "red step pixel {x},{x} reads {}", coverage(&r, x, x));
        assert_eq!(coverage(&r, x, x - 1), 255, "black stays fully selected");
    }
}

#[test]
fn a_slope_of_one_in_four_ramps_across_each_run() {
    let img = slope();
    let r = wand_region(&img, AREA, (30, 45), 32.0, true, true).unwrap();
    // The run at x 16–19 (y 23): one row up at its left, one row down at its right.
    let got: Vec<u8> = (16..20).map(|x| coverage(&r, x, 23)).collect();
    assert_eq!(got, [223, 159, 96, 32]);
    assert_eq!(coverage(&r, 16, 24), 255, "the pixel below stays fully selected");
}

#[test]
fn a_rectangle_and_an_l_shape_get_no_stair_coverage() {
    let mut rect = blank([0; 4]);
    fill(&mut rect, 10, 10, 30, 25);
    let r = wand_region(&rect, AREA, (15, 15), 32.0, true, true).unwrap();
    assert_eq!(partial(&r), 0, "rectangle");

    let mut l = blank([0; 4]);
    fill(&mut l, 10, 30, 50, 40);
    fill(&mut l, 10, 10, 20, 30);
    let r = wand_region(&l, AREA, (15, 35), 32.0, true, true).unwrap();
    assert_eq!(partial(&r), 0, "L shape");
    assert_eq!(coverage(&r, 20, 29), 0, "the inner corner: the step is more than one row");
}

#[test]
fn the_8_bit_and_f32_wands_draw_the_same_stair_coverage() {
    for (img, seed) in [(triangle([0; 4]), (30, 35)), (slope(), (30, 45))] {
        let f: Vec<[f32; 4]> = img.iter().map(|p| p.map(|v| f32::from(v) / 255.0)).collect();
        for contiguous in [true, false] {
            let r = wand_region(&img, AREA, seed, 32.0, contiguous, true).unwrap();
            let m = magic_wand(&f, AREA, seed, 32.0, contiguous, true);
            let mut soft = 0;
            for y in 0..H as i32 {
                for x in 0..W as i32 {
                    let want = (m[y as usize * W + x as usize] * 255.0).round() as u8;
                    assert_eq!(coverage(&r, x, y), want, "contiguous {contiguous} at {x},{y}");
                    soft += usize::from(!matches!(want, 0 | 255));
                }
            }
            assert!(soft > 0, "the image has a staircase to soften");
        }
    }
}

#[test]
fn anti_alias_off_has_no_partly_selected_pixels() {
    let img = triangle([0; 4]);
    let r = wand_region(&img, AREA, (30, 35), 32.0, true, false).unwrap();
    assert_eq!(partial(&r), 0);
    let img = slope();
    let r = wand_region(&img, AREA, (30, 45), 32.0, true, false).unwrap();
    assert_eq!(partial(&r), 0);
}
