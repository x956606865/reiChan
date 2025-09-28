use image::GenericImageView;
use image::{DynamicImage, ImageBuffer, Luma};
use opencv::{core, imgproc, prelude::*};

#[derive(Debug, Clone)]
pub struct MaskResult {
    pub mask: ImageBuffer<Luma<u8>, Vec<u8>>,
    pub foreground_ratio: f32,
    pub bounding_box: Option<BoundingBox>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BoundingBox {
    pub x0: u32,
    pub y0: u32,
    pub x1: u32,
    pub y1: u32,
}

impl BoundingBox {
    pub fn width(&self) -> u32 {
        self.x1 - self.x0
    }

    pub fn height(&self) -> u32 {
        self.y1 - self.y0
    }
}

pub fn build_foreground_mask(image: &DynamicImage) -> opencv::Result<MaskResult> {
    let (width, height) = image.dimensions();
    if width == 0 || height == 0 {
        return Ok(MaskResult {
            mask: ImageBuffer::new(width, height),
            foreground_ratio: 0.0,
            bounding_box: None,
        });
    }

    let gray = image.to_luma8();
    let gray_vec = gray.into_raw();

    let gray_mat = core::Mat::from_slice(&gray_vec)?;
    let gray_mat = gray_mat.reshape(1, height as i32)?;

    let mut blurred = core::Mat::default();
    imgproc::gaussian_blur(
        &gray_mat,
        &mut blurred,
        core::Size::new(5, 5),
        0.0,
        0.0,
        core::BORDER_REPLICATE,
        core::AlgorithmHint::ALGO_HINT_DEFAULT,
    )?;

    let mut clahe = imgproc::create_clahe(2.0, core::Size::new(8, 8))?;
    let mut equalized = core::Mat::default();
    clahe.apply(&blurred, &mut equalized)?;

    let mut binary = core::Mat::default();
    imgproc::threshold(
        &equalized,
        &mut binary,
        0.0,
        255.0,
        imgproc::THRESH_BINARY_INV | imgproc::THRESH_OTSU,
    )?;

    let kernel = imgproc::get_structuring_element(
        imgproc::MORPH_RECT,
        core::Size::new(5, 5),
        core::Point::new(-1, -1),
    )?;

    let mut opened = core::Mat::default();
    imgproc::morphology_ex(
        &binary,
        &mut opened,
        imgproc::MORPH_OPEN,
        &kernel,
        core::Point::new(-1, -1),
        1,
        core::BORDER_CONSTANT,
        core::Scalar::default(),
    )?;

    let mut cleaned = core::Mat::default();
    imgproc::morphology_ex(
        &opened,
        &mut cleaned,
        imgproc::MORPH_CLOSE,
        &kernel,
        core::Point::new(-1, -1),
        1,
        core::BORDER_CONSTANT,
        core::Scalar::default(),
    )?;

    let mask_bytes = cleaned.data_bytes()?.to_vec();
    let mut bbox: Option<BoundingBox> = None;
    let mut foreground_pixels = 0u32;
    let width_usize = width as usize;

    for (index, value) in mask_bytes.iter().enumerate() {
        if *value > 0 {
            foreground_pixels += 1;
            let x = (index % width_usize) as u32;
            let y = (index / width_usize) as u32;
            bbox = Some(match bbox {
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

    let mask = ImageBuffer::<Luma<u8>, Vec<u8>>::from_vec(width, height, mask_bytes)
        .expect("mask buffer size mismatch");

    let total_pixels = (width * height) as f32;
    let foreground_ratio = if total_pixels == 0.0 {
        0.0
    } else {
        foreground_pixels as f32 / total_pixels
    };

    Ok(MaskResult {
        mask,
        foreground_ratio,
        bounding_box: bbox,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::DynamicImage;

    #[test]
    fn detects_simple_foreground_rect() {
        let width = 64;
        let height = 48;
        let mut buffer =
            image::ImageBuffer::from_pixel(width, height, image::Rgb([255u8, 255, 255]));
        for y in 10..38 {
            for x in 20..44 {
                *buffer.get_pixel_mut(x, y) = image::Rgb([0, 0, 0]);
            }
        }
        let image = DynamicImage::ImageRgb8(buffer);

        let result = build_foreground_mask(&image).expect("mask computation should succeed");
        let bbox = result.bounding_box.expect("bbox expected");

        assert_eq!(bbox.x0, 20);
        assert_eq!(bbox.y0, 10);
        assert_eq!(bbox.x1, 44);
        assert_eq!(bbox.y1, 38);

        let expected_ratio = ((44 - 20) * (38 - 10)) as f32 / (width * height) as f32;
        assert!((result.foreground_ratio - expected_ratio).abs() < 0.05);

        assert_eq!(result.mask.get_pixel(22, 12)[0], 255);
        assert_eq!(result.mask.get_pixel(5, 5)[0], 0);
    }
}
