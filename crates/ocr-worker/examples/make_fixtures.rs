//! Portable, Rust-only input fixtures for the smoke script and OCR experiments.
use std::{io::Write, path::PathBuf};

fn main() -> anyhow::Result<()> {
    let directory = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("fixtures"));
    std::fs::create_dir_all(&directory)?;
    let mut image = image::RgbImage::from_pixel(720, 160, image::Rgb([255; 3]));
    let text = "OCR TEST 123";
    for (index, letter) in text.chars().enumerate() {
        for (row, pattern) in glyph(letter).iter().enumerate() {
            for column in 0..5 {
                if pattern & (1 << (4 - column)) == 0 {
                    continue;
                }
                for dy in 0..8 {
                    for dx in 0..8 {
                        image.put_pixel(
                            32 + index as u32 * 48 + column * 8 + dx,
                            40 + row as u32 * 8 + dy,
                            image::Rgb([0; 3]),
                        );
                    }
                }
            }
        }
    }
    image.save(directory.join("fixture.png"))?;
    std::fs::write(directory.join("fixture.pdf"), pdf_fixture()?)?;
    println!(
        "Created {} and {}",
        directory.join("fixture.png").display(),
        directory.join("fixture.pdf").display()
    );
    Ok(())
}

fn glyph(letter: char) -> [u8; 7] {
    match letter {
        'O' => [14, 17, 17, 17, 17, 17, 14],
        'C' => [14, 17, 16, 16, 16, 17, 14],
        'R' => [30, 17, 17, 30, 20, 18, 17],
        'T' => [31, 4, 4, 4, 4, 4, 4],
        'E' => [31, 16, 16, 30, 16, 16, 31],
        'S' => [15, 16, 16, 14, 1, 1, 30],
        '1' => [4, 12, 4, 4, 4, 4, 14],
        '2' => [14, 17, 1, 2, 4, 8, 31],
        '3' => [30, 1, 1, 14, 1, 1, 30],
        _ => [0; 7],
    }
}

fn pdf_fixture() -> std::io::Result<Vec<u8>> {
    let content1 = "BT /F1 28 Tf 72 700 Td (OCR SERVICE TEST PAGE 1) Tj ET\n";
    let content2 = "BT /F1 28 Tf 72 700 Td (OCR SERVICE TEST PAGE 2) Tj ET\n";
    let objects = [
        "<< /Type /Catalog /Pages 2 0 R >>".into(),
        "<< /Type /Pages /Kids [3 0 R 5 0 R] /Count 2 >>".into(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 7 0 R >> >> /Contents 4 0 R >>".into(),
        format!("<< /Length {} >>\nstream\n{}endstream", content1.len(), content1),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 7 0 R >> >> /Contents 6 0 R >>".into(),
        format!("<< /Length {} >>\nstream\n{}endstream", content2.len(), content2),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".into(),
    ];
    let mut output = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    for (index, object) in objects.iter().enumerate() {
        offsets.push(output.len());
        writeln!(output, "{} 0 obj\n{}\nendobj", index + 1, object)?;
    }
    let xref = output.len();
    writeln!(output, "xref\n0 {}\n0000000000 65535 f ", objects.len() + 1)?;
    for offset in offsets {
        writeln!(output, "{offset:010} 00000 n ")?;
    }
    writeln!(
        output,
        "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF",
        objects.len() + 1
    )?;
    Ok(output)
}
