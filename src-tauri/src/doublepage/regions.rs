use image::{DynamicImage, GenericImageView, ImageBuffer, Luma};

use super::mask::BoundingBox;

#[derive(Debug, Clone, Copy)]
pub struct RegionBounds {
    pub bbox: BoundingBox,
}

pub fn compute_region_bbox(
    mask: &ImageBuffer<Luma<u8>, Vec<u8>>,
    x_start: u32,
    x_end: u32,
) -> RegionBounds {
    let (width, height) = mask.dimensions();
    if width == 0 || height == 0 {
        return RegionBounds {
            bbox: BoundingBox {
                x0: 0,
                y0: 0,
                x1: 0,
                y1: 0,
            },
        };
    }

    let clamped_start = x_start.min(width.saturating_sub(1));
    let clamped_end = x_end.min(width);
    let effective_end = clamped_end.max(clamped_start + 1);

    let mut bounds: Option<BoundingBox> = None;

    for y in 0..height {
        for x in clamped_start..effective_end {
            if mask.get_pixel(x, y)[0] == 0 {
                continue;
            }

            bounds = Some(match bounds {
                None => BoundingBox {
                    x0: x,
                    y0: y,
                    x1: x + 1,
                    y1: y + 1,
                },
                Some(mut current) => {
                    if x < current.x0 {
                        current.x0 = x;
                    }
                    if y < current.y0 {
                        current.y0 = y;
                    }
                    if x + 1 > current.x1 {
                        current.x1 = x + 1;
                    }
                    if y + 1 > current.y1 {
                        current.y1 = y + 1;
                    }
                    current
                }
            });
        }
    }

    let bbox = bounds.unwrap_or(BoundingBox {
        x0: clamped_start,
        y0: 0,
        x1: effective_end,
        y1: height,
    });

    RegionBounds { bbox }
}

pub fn crop_region_with_padding(
    image: &DynamicImage,
    bounds: &RegionBounds,
    padding_x: u32,
    padding_y: u32,
) -> DynamicImage {
    let (width, height) = image.dimensions();
    let bbox = bounds.bbox;

    let x0 = bbox
        .x0
        .saturating_sub(padding_x)
        .min(width.saturating_sub(1));
    let y0 = bbox
        .y0
        .saturating_sub(padding_y)
        .min(height.saturating_sub(1));
    let x1 = (bbox.x1 + padding_x).min(width);
    let y1 = (bbox.y1 + padding_y).min(height);

    if x1 <= x0 || y1 <= y0 {
        return image.crop_imm(0, 0, width, height);
    }

    image.crop_imm(x0, y0, x1 - x0, y1 - y0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::DynamicImage;

    fn make_mask(width: u32, height: u32, fill: &[(u32, u32)]) -> ImageBuffer<Luma<u8>, Vec<u8>> {
        let mut mask = ImageBuffer::from_pixel(width, height, Luma([0u8]));
        for &(x, y) in fill.iter() {
            mask.put_pixel(x, y, Luma([255]));
        }
        mask
    }

    #[test]
    fn compute_region_bbox_returns_slice_bounds_when_empty() {
        let mask = ImageBuffer::from_pixel(10, 6, Luma([0u8]));
        let region = compute_region_bbox(&mask, 2, 8);
        assert_eq!(region.bbox.x0, 2);
        assert_eq!(region.bbox.x1, 8);
        assert_eq!(region.bbox.y0, 0);
        assert_eq!(region.bbox.y1, 6);
    }

    #[test]
    fn compute_region_bbox_tracks_foreground_pixels() {
        let mask = make_mask(12, 10, &[(3, 4), (5, 6), (7, 3)]);
        let region = compute_region_bbox(&mask, 2, 10);
        assert_eq!(region.bbox.x0, 3);
        assert_eq!(region.bbox.x1, 8);
        assert_eq!(region.bbox.y0, 3);
        assert_eq!(region.bbox.y1, 7);
    }

    #[test]
    fn crop_region_with_padding_respects_bounds() {
        let mut image = image::ImageBuffer::from_pixel(20, 10, image::Rgb([255u8, 255, 255]));
        for x in 5..15 {
            for y in 2..8 {
                *image.get_pixel_mut(x, y) = image::Rgb([0, 0, 0]);
            }
        }
        let image = DynamicImage::ImageRgb8(image);
        let mask = make_mask(20, 10, &[(6, 3), (14, 7)]);
        let region = compute_region_bbox(&mask, 5, 15);
        let cropped = crop_region_with_padding(&image, &region, 1, 1);
        assert_eq!(cropped.width(), 11);
        assert_eq!(cropped.height(), 7);
    }
}
