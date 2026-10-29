//! PDF mínimo gerado nos testes (spec 134/T02): uma página por texto, fonte Helvetica padrão
//! (sem fonte embutida) e codificação WinAnsi, com a tabela `xref` calculada. Nenhum PDF de
//! terceiros entra no repositório.

/// Texto de uma linha na sintaxe de string do PDF, em WinAnsi (Latin-1 para os acentos).
fn pdf_string(line: &str) -> Vec<u8> {
    let mut out = vec![b'('];
    for ch in line.chars() {
        match ch {
            '(' | ')' | '\\' => {
                out.push(b'\\');
                out.push(ch as u8);
            }
            c if c.is_ascii() && !c.is_ascii_control() => out.push(c as u8),
            c if (c as u32) >= 0xA0 && (c as u32) <= 0xFF => out.extend(format!("\\{:03o}", c as u32).bytes()),
            _ => out.push(b'?'),
        }
    }
    out.push(b')');
    out
}

/// Quebra o texto em linhas de até 80 caracteres (as quebras de linha do texto valem).
fn wrap(text: &str) -> Vec<String> {
    let mut lines = Vec::new();
    for para in text.split('\n') {
        let mut line = String::new();
        for word in para.split_whitespace() {
            if !line.is_empty() && line.chars().count() + word.chars().count() > 80 {
                lines.push(std::mem::take(&mut line));
            }
            if !line.is_empty() {
                line.push(' ');
            }
            line.push_str(word);
        }
        lines.push(line);
    }
    lines
}

/// PDF com uma página por item de `pages`. Página vazia não tem operador de texto (como uma
/// página escaneada sem camada de texto).
pub fn pdf(pages: &[&str]) -> Vec<u8> {
    pdf_with(pages, false)
}

/// Como [`pdf`]; com `hex`, o texto vai em strings hexadecimais (`<4F6C61> Tj`), sem aparecer
/// literal nos bytes do arquivo.
pub fn pdf_with(pages: &[&str], hex: bool) -> Vec<u8> {
    assemble(page_objects(pages, hex), "")
}

/// PDF protegido por senha (dicionário `/Encrypt` padrão, RC4 de 40 bits, revisão 2) cuja senha
/// de usuário não é a vazia: não abre sem a senha.
pub fn encrypted(pages: &[&str]) -> Vec<u8> {
    let mut objects = page_objects(pages, false);
    let hash = "(01234567890123456789012345678901)";
    objects.push(format!("<< /Filter /Standard /V 1 /R 2 /Length 40 /P -1 /O {hash} /U {hash} >>").into_bytes());
    let encrypt = objects.len();
    assemble(objects, &format!(" /Encrypt {encrypt} 0 R /ID [(0123456789abcdef) (0123456789abcdef)]"))
}

/// Catálogo, árvore de páginas, fonte e, por página, a página e o conteúdo.
fn page_objects(pages: &[&str], hex: bool) -> Vec<Vec<u8>> {
    let mut objects: Vec<Vec<u8>> = Vec::new();
    let page_ids: Vec<usize> = (0..pages.len()).map(|i| 4 + 2 * i).collect();
    objects.push(b"<< /Type /Catalog /Pages 2 0 R >>".to_vec());
    let kids: Vec<String> = page_ids.iter().map(|id| format!("{id} 0 R")).collect();
    objects.push(format!("<< /Type /Pages /Kids [{}] /Count {} >>", kids.join(" "), pages.len()).into_bytes());
    objects.push(b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>".to_vec());
    for (i, text) in pages.iter().enumerate() {
        let content_id = 5 + 2 * i;
        objects.push(
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 3 0 R >> >> /Contents {content_id} 0 R >>"
            )
            .into_bytes(),
        );
        let mut stream: Vec<u8> = Vec::new();
        if text.trim().is_empty() {
            stream.extend_from_slice(b"q Q");
        } else {
            stream.extend_from_slice(b"BT /F1 11 Tf 14 TL 72 740 Td ");
            for (n, line) in wrap(text).iter().enumerate() {
                if n > 0 {
                    stream.extend_from_slice(b"T* ");
                }
                let literal = pdf_string(line);
                if hex {
                    let inner = &literal[1..literal.len() - 1];
                    let mut raw = Vec::new();
                    let mut k = 0;
                    while k < inner.len() {
                        if inner[k] == b'\\' && k + 3 < inner.len() && inner[k + 1].is_ascii_digit() {
                            let oct = std::str::from_utf8(&inner[k + 1..k + 4]).ok().and_then(|o| u8::from_str_radix(o, 8).ok());
                            raw.push(oct.unwrap_or(b'?'));
                            k += 4;
                        } else if inner[k] == b'\\' && k + 1 < inner.len() {
                            raw.push(inner[k + 1]);
                            k += 2;
                        } else {
                            raw.push(inner[k]);
                            k += 1;
                        }
                    }
                    stream.push(b'<');
                    stream.extend(raw.iter().flat_map(|b| format!("{b:02X}").into_bytes()));
                    stream.push(b'>');
                } else {
                    stream.extend(literal);
                }
                stream.extend_from_slice(b" Tj ");
            }
            stream.extend_from_slice(b"ET");
        }
        let mut obj = format!("<< /Length {} >>\nstream\n", stream.len()).into_bytes();
        obj.extend(stream);
        obj.extend_from_slice(b"\nendstream");
        objects.push(obj);
    }
    objects
}

/// Arquivo completo: cabeçalho, objetos, `xref` e trailer (com `trailer_extra` no dicionário).
fn assemble(objects: Vec<Vec<u8>>, trailer_extra: &str) -> Vec<u8> {
    let mut out = b"%PDF-1.4\n%\xe2\xe3\xcf\xd3\n".to_vec();
    let mut offsets = Vec::with_capacity(objects.len());
    for (i, obj) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.extend(format!("{} 0 obj\n", i + 1).into_bytes());
        out.extend_from_slice(obj);
        out.extend_from_slice(b"\nendobj\n");
    }
    let xref_at = out.len();
    out.extend(format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).into_bytes());
    for offset in offsets {
        out.extend(format!("{offset:010} 00000 n \n").into_bytes());
    }
    out.extend(format!("trailer\n<< /Size {} /Root 1 0 R{trailer_extra} >>\nstartxref\n{xref_at}\n%%EOF\n", objects.len() + 1).into_bytes());
    out
}
