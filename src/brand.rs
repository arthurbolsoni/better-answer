//! O desenho da marca: quadrado azul de cantos arredondados com tres barras brancas.
//!
//! Mora fora do resto do app de proposito. O `build.rs` faz `include!` deste arquivo para gerar o
//! `.ico` que vai embutido no `.exe`, e o app chama a mesma funcao para o icone da bandeja: um
//! desenho, dois consumidores, sem versao "do executavel" para sair de sincronia. Por isso aqui
//! nao entra nenhum `use` nem nada do resto do crate — no build script nao existe crate do app
//! para resolver caminho.

/// RGBA (8 bits por canal, sem pre-multiplicacao) de `size` x `size`, de cima para baixo.
///
/// As medidas sao fracoes do lado, calibradas nos 32px da bandeja: raio 6, barras de 3px nas
/// linhas 9/15/21 comecando na coluna 7. Em `size == 32` o resultado e identico ao que a bandeja
/// desenhava antes; nos tamanhos pequenos a barra cai para 1px sozinha, que e o que ainda da
/// leitura em 16px.
pub fn rgba(size: u32) -> Vec<u8> {
    let s = size as i32;
    let scale = |num: i32| ((s * num) as f32 / 32.0).round() as i32;

    let radius = scale(6);
    // Meia espessura da barra: 3/32 do lado, arredondado para o inteiro mais proximo.
    let half = (((s as f32 * 3.0 / 32.0) - 1.0) / 2.0).round().max(0.0) as i32;

    let bars = [
        (scale(9), scale(7), scale(25)),
        (scale(15), scale(7), scale(21)),
        (scale(21), scale(7), scale(17)),
    ];

    let mut rgba = vec![0u8; (size * size * 4) as usize];
    for y in 0..s {
        for x in 0..s {
            let index = ((y * s + x) * 4) as usize;

            // Canto arredondado: fora do raio nos cantos, deixa transparente.
            let cx = if x < radius {
                radius - x
            } else if x >= s - radius {
                x - (s - radius - 1)
            } else {
                0
            };
            let cy = if y < radius {
                radius - y
            } else if y >= s - radius {
                y - (s - radius - 1)
            } else {
                0
            };
            if cx * cx + cy * cy > radius * radius {
                continue;
            }

            let on_bar = bars
                .iter()
                .any(|(by, x0, x1)| (y - by).abs() <= half && x >= *x0 && x <= *x1);

            let (r, g, b) = if on_bar {
                (255, 255, 255)
            } else {
                (47, 107, 255)
            };
            rgba[index] = r;
            rgba[index + 1] = g;
            rgba[index + 2] = b;
            rgba[index + 3] = 255;
        }
    }
    rgba
}
