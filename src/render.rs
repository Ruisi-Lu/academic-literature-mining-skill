use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use image::codecs::jpeg::JpegEncoder;
use image::{ExtendedColorType, ImageEncoder};
use pdfium_auto::bind_pdfium;
use pdfium_render::prelude::*;
use tracing::warn;
use uuid::Uuid;

use crate::config::Settings;
use crate::domain::PageRecord;
use crate::util::{safe_slug, sha256_bytes, sha256_file};

const PAGE_NAMESPACE: Uuid = Uuid::from_u128(0x264e_568e_093f_4cf1_877e_a1c2_b56b_1eb6);

pub fn render_pdf(
    settings: &Settings,
    workspace: &Path,
    work_id: &str,
    pdf_path: &Path,
) -> Result<Vec<PageRecord>> {
    let pdfium = bind_pdfium(Some(&|downloaded, total| {
        if let Some(total) = total {
            eprint!("\rDownloading PDFium: {downloaded}/{total} bytes");
        }
    }))
    .context("initialize PDFium from Rust")?;
    let document = pdfium
        .load_pdf_from_file(pdf_path, None)
        .with_context(|| format!("load PDF {}", pdf_path.display()))?;
    let short_id = safe_slug(work_id, "paper");
    let id_hash = &sha256_bytes(work_id.as_bytes())[..12];
    let output_dir = workspace
        .join("pages")
        .join(format!("{short_id}-{id_hash}"));
    if output_dir.exists() {
        fs::remove_dir_all(&output_dir)?;
    }
    fs::create_dir_all(&output_dir)?;
    let target_width = (settings.render_dpi as i32 * 17 / 2).max(1000);
    let max_height = (settings.render_dpi as i32 * 14).max(1400);
    let config = PdfRenderConfig::new()
        .set_target_width(target_width)
        .set_maximum_height(max_height);
    let mut pages = Vec::with_capacity(document.pages().len() as usize);
    for (index, page) in document.pages().iter().enumerate() {
        let page_number = index as u32 + 1;
        let page_text = match page.text() {
            Ok(text) => normalize_page_text(&text.all()),
            Err(error) => {
                warn!(
                    page_number,
                    %error,
                    "PDF native text extraction failed; using image-only input for this page"
                );
                String::new()
            }
        };
        let image = page
            .render_with_config(&config)
            .with_context(|| format!("render page {}", index + 1))?
            .as_image();
        let rgb = image.into_rgb8();
        let image_path: PathBuf = output_dir.join(format!("{page_number:05}.jpg"));
        let file = fs::File::create(&image_path)?;
        let encoder = JpegEncoder::new_with_quality(file, settings.jpeg_quality);
        encoder.write_image(
            rgb.as_raw(),
            rgb.width(),
            rgb.height(),
            ExtendedColorType::Rgb8,
        )?;
        let page_id = Uuid::new_v5(
            &PAGE_NAMESPACE,
            format!("{work_id}\u{1f}{page_number}").as_bytes(),
        )
        .to_string();
        pages.push(PageRecord {
            page_id,
            work_id: work_id.to_owned(),
            page_number,
            image_path: image_path.to_string_lossy().into_owned(),
            image_sha256: sha256_file(&image_path)?,
            page_text,
            width: rgb.width(),
            height: rgb.height(),
            indexed_at: None,
        });
    }
    Ok(pages)
}

fn normalize_page_text(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    #[ignore = "requires a prepared PDFium native library"]
    fn prepares_a_complete_pdf_page_with_native_text_and_image() {
        let temporary = tempdir().unwrap();
        let workspace = temporary.path().join("corpus");
        fs::create_dir_all(workspace.join("pdfs")).unwrap();
        let pdf_path = workspace.join("pdfs").join("smoke.pdf");
        fs::write(&pdf_path, minimal_pdf()).unwrap();
        let mut settings = Settings::load(None).unwrap();
        settings.render_dpi = 72;
        settings.jpeg_quality = 80;
        let pages = render_pdf(&settings, &workspace, "doi:10.1000/smoke", &pdf_path).unwrap();
        assert_eq!(pages.len(), 1);
        assert_eq!(pages[0].page_number, 1);
        assert!(Path::new(&pages[0].image_path).is_file());
        assert!(!pages[0].image_sha256.is_empty());
        assert_eq!(pages[0].page_text, "Academic PDF smoke test");
    }

    #[test]
    fn normalizes_pdf_control_characters_and_whitespace() {
        assert_eq!(
            normalize_page_text("  Native\u{0000} PDF\r\n text\tlayer  "),
            "Native PDF text layer"
        );
    }

    fn minimal_pdf() -> Vec<u8> {
        let stream = b"BT /F1 24 Tf 72 720 Td (Academic PDF smoke test) Tj ET";
        let objects = [
            "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>".to_owned(),
            format!(
                "<< /Length {} >>\nstream\n{}\nendstream",
                stream.len(),
                String::from_utf8_lossy(stream)
            ),
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_owned(),
        ];
        let mut pdf = b"%PDF-1.4\n%\xE2\xE3\xCF\xD3\n".to_vec();
        let mut offsets = Vec::new();
        for (index, object) in objects.iter().enumerate() {
            offsets.push(pdf.len());
            pdf.extend_from_slice(format!("{} 0 obj\n{object}\nendobj\n", index + 1).as_bytes());
        }
        let xref = pdf.len();
        pdf.extend_from_slice(format!("xref\n0 {}\n", objects.len() + 1).as_bytes());
        pdf.extend_from_slice(b"0000000000 65535 f \n");
        for offset in offsets {
            pdf.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        pdf.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
                objects.len() + 1
            )
            .as_bytes(),
        );
        pdf
    }
}
