use super::*;

// Self-authored, uncompressed test PDFs. No user article, OCR or external
// document is involved; Poppler still parses and renders the actual PDF bytes.
fn synthetic_pdf(page_streams: &[&str]) -> Vec<u8> {
    let image_id = 4 + page_streams.len() * 2;
    let children = (0..page_streams.len())
        .map(|index| format!("{} 0 R", 4 + index * 2))
        .collect::<Vec<_>>()
        .join(" ");
    let mut objects = vec![
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        format!(
            "<< /Type /Pages /Kids [{children}] /Count {} >>",
            page_streams.len()
        )
        .into_bytes(),
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec(),
    ];
    for (index, stream) in page_streams.iter().enumerate() {
        objects.push(format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 3 0 R >> /XObject << /Scan {image_id} 0 R >> >> /Contents {} 0 R >>", 5 + index * 2).into_bytes());
        objects.push(
            format!(
                "<< /Length {} >>\nstream\n{stream}\nendstream",
                stream.len()
            )
            .into_bytes(),
        );
    }
    let mut image = b"<< /Type /XObject /Subtype /Image /Width 1 /Height 1 /ColorSpace /DeviceRGB /BitsPerComponent 8 /Length 3 >>\nstream\n".to_vec();
    image.extend_from_slice(&[0, 0, 0]);
    image.extend_from_slice(b"\nendstream");
    objects.push(image);
    let mut pdf = b"%PDF-1.4\n".to_vec();
    let mut offsets = vec![0];
    for (index, object) in objects.iter().enumerate() {
        offsets.push(pdf.len());
        pdf.extend_from_slice(format!("{} 0 obj\n", index + 1).as_bytes());
        pdf.extend_from_slice(object);
        pdf.extend_from_slice(b"\nendobj\n");
    }
    let xref = pdf.len();
    pdf.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", offsets.len()).as_bytes());
    for offset in &offsets[1..] {
        pdf.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    pdf.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            offsets.len()
        )
        .as_bytes(),
    );
    pdf
}

async fn extract_sample(page_streams: &[&str]) -> CoreResult<ExtractedDocument> {
    let file = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(file.path(), synthetic_pdf(page_streams)).unwrap();
    extract(file.path()).await
}

#[tokio::test]
async fn blank_pages_and_short_cover_tail_keep_original_page_numbers() {
    let document = extract_sample(&[
        "",
        "BT /F1 18 Tf 60 740 Td (Cover) Tj ET",
        "",
        "BT /F1 12 Tf 60 740 Td (This synthetic article checks page mapping.) Tj ET",
        "",
        "BT /F1 12 Tf 60 740 Td (End) Tj ET",
        "",
    ])
    .await
    .expect("verified empty and short text pages are supported");
    for (quote, page) in [("Cover", 2), ("synthetic", 4), ("End", 6)] {
        let byte_start = document.text.find(quote).unwrap();
        let start = document.text[..byte_start].chars().count();
        let location = document
            .location(start, start + quote.chars().count(), quote)
            .unwrap();
        assert_eq!(location.page, Some(page));
        assert_eq!(location.line_start, Some(1));
        assert_eq!(
            document
                .text
                .chars()
                .skip(start)
                .take(quote.chars().count())
                .collect::<String>(),
            quote
        );
    }
}

#[tokio::test]
async fn a_short_text_document_and_spaces_are_not_scanned() {
    // Give the PDF's two words enough physical separation. Poppler determines
    // word spaces from glyph positions rather than literal stream whitespace.
    let document = extract_sample(&["BT /F1 12 Tf 60 740 Td (A  B) Tj ET"])
        .await
        .expect("a short genuine text page");
    assert_eq!(
        document.text.split_whitespace().collect::<Vec<_>>(),
        ["A", "B"]
    );
}

#[tokio::test]
async fn scans_and_unextracted_outlines_still_fail_with_text_covers_or_page_numbers() {
    for page in [
        "q 400 0 0 600 60 100 cm /Scan Do Q",
        "BT /F1 12 Tf 60 740 Td (1) Tj ET q 400 0 0 600 60 100 cm /Scan Do Q",
        "0 0 0 rg 60 100 200 300 re f",
        "BT /F1 12 Tf 60 740 Td (1) Tj ET 0 0 0 rg 60 100 200 300 re f",
    ] {
        let error = extract_sample(&[
            "BT /F1 12 Tf 60 740 Td (This is a genuine text cover.) Tj ET",
            page,
        ])
        .await
        .expect_err("ambiguous painted pages cannot be skipped");
        assert_eq!(error.code(), ErrorCode::PdfScanned, "{page}");
        assert!(error.safe_message().contains("第 2 页"));
    }
}

#[tokio::test]
async fn all_empty_and_damaged_documents_are_rejected() {
    assert_eq!(
        extract_sample(&["", ""]).await.unwrap_err().code(),
        ErrorCode::PdfScanned
    );
    let file = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(file.path(), b"%PDF-1.4\nbroken synthetic document").unwrap();
    assert_eq!(
        extract(file.path()).await.unwrap_err().code(),
        ErrorCode::ExtractionFailed
    );
}
