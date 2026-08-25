//! Embute o icone no `.exe`.
//!
//! O desenho vem do mesmo `src/brand.rs` que a bandeja usa, montado aqui como modulo por
//! `#[path]` — build script e outro crate, nao alcanca o modulo do app. Daqui sai um `.ico` no
//! `OUT_DIR` e um `.rc` de uma linha; o `embed-resource` chama o `rc.exe` do Windows SDK (ou o
//! `llvm-rc`) e linka o recurso. Sem isso o Explorer, o Menu Iniciar e a barra de tarefas mostram
//! o icone generico de aplicativo, porque o binario nao tem recurso `ICON` nenhum.

#[path = "src/brand.rs"]
mod brand;

use std::path::PathBuf;

/// Tamanhos que entram no `.ico`. 16/20/24/32 sao os que o shell pede na pratica; 48 e a "icone
/// grande" do Explorer, 256 e o que a visualizacao extra grande e o Alt+Tab ampliam.
const SIZES: [u32; 9] = [16, 20, 24, 32, 40, 48, 64, 128, 256];

fn main() {
    println!("cargo:rerun-if-changed=src/brand.rs");
    println!("cargo:rerun-if-changed=build.rs");

    let out = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
    let ico = out.join("better-answer.ico");
    std::fs::write(&ico, ico_bytes()).expect("gravar o .ico");

    let rc = out.join("better-answer.rc");
    // O `rc.exe` trata `\` como escape dentro da string, entao o caminho vai com `/`.
    std::fs::write(
        &rc,
        format!("1 ICON \"{}\"\n", ico.display().to_string().replace('\\', "/")),
    )
    .expect("gravar o .rc");

    let _ = embed_resource::compile(&rc, embed_resource::NONE);
}

/// Monta o `.ico`: cabecalho, uma entrada por tamanho e as imagens em BMP 32 bits.
fn ico_bytes() -> Vec<u8> {
    let images: Vec<Vec<u8>> = SIZES.iter().map(|size| bmp_image(*size)).collect();

    let mut out = Vec::new();
    out.extend_from_slice(&0u16.to_le_bytes()); // reservado
    out.extend_from_slice(&1u16.to_le_bytes()); // tipo: 1 = icone
    out.extend_from_slice(&(SIZES.len() as u16).to_le_bytes());

    // O offset da primeira imagem vem depois de todas as entradas do diretorio.
    let mut offset = 6 + 16 * SIZES.len() as u32;
    for (size, image) in SIZES.iter().zip(&images) {
        // 256 nao cabe em u8: o formato representa esse tamanho como 0.
        let dim = if *size >= 256 { 0u8 } else { *size as u8 };
        out.push(dim); // largura
        out.push(dim); // altura
        out.push(0); // cores da paleta (0 = sem paleta)
        out.push(0); // reservado
        out.extend_from_slice(&1u16.to_le_bytes()); // planos
        out.extend_from_slice(&32u16.to_le_bytes()); // bits por pixel
        out.extend_from_slice(&(image.len() as u32).to_le_bytes());
        out.extend_from_slice(&offset.to_le_bytes());
        offset += image.len() as u32;
    }
    for image in images {
        out.extend_from_slice(&image);
    }
    out
}

/// Uma imagem do `.ico`: `BITMAPINFOHEADER` + pixels BGRA de baixo para cima + mascara AND.
///
/// A mascara e redundante com o canal alfa em 32 bits, mas o campo existe no formato e alguns
/// caminhos antigos do shell ainda a leem — vai coerente com o alfa em vez de zerada.
fn bmp_image(size: u32) -> Vec<u8> {
    let rgba = brand::rgba(size);
    let s = size as i32;

    let mut out = Vec::new();
    out.extend_from_slice(&40u32.to_le_bytes()); // biSize
    out.extend_from_slice(&(s).to_le_bytes()); // biWidth
    out.extend_from_slice(&(s * 2).to_le_bytes()); // biHeight: imagem + mascara
    out.extend_from_slice(&1u16.to_le_bytes()); // biPlanes
    out.extend_from_slice(&32u16.to_le_bytes()); // biBitCount
    out.extend_from_slice(&0u32.to_le_bytes()); // biCompression: BI_RGB
    out.extend_from_slice(&0u32.to_le_bytes()); // biSizeImage
    out.extend_from_slice(&0u32.to_le_bytes()); // biXPelsPerMeter
    out.extend_from_slice(&0u32.to_le_bytes()); // biYPelsPerMeter
    out.extend_from_slice(&0u32.to_le_bytes()); // biClrUsed
    out.extend_from_slice(&0u32.to_le_bytes()); // biClrImportant

    for y in (0..s).rev() {
        for x in 0..s {
            let index = ((y * s + x) * 4) as usize;
            out.push(rgba[index + 2]); // B
            out.push(rgba[index + 1]); // G
            out.push(rgba[index]); // R
            out.push(rgba[index + 3]); // A
        }
    }

    // Mascara AND: 1 bit por pixel, bit 1 = transparente, linha alinhada em 4 bytes.
    let stride = ((size + 31) / 32 * 4) as usize;
    for y in (0..s).rev() {
        let mut row = vec![0u8; stride];
        for x in 0..s {
            if rgba[((y * s + x) * 4 + 3) as usize] == 0 {
                row[(x / 8) as usize] |= 0x80 >> (x % 8);
            }
        }
        out.extend_from_slice(&row);
    }
    out
}
