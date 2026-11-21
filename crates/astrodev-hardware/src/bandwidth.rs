//! Ancho de banda de memoria (GB/s): la generación de tokens está limitada por él.

/// Tabla de GPUs conocidas (subcadena del nombre en minúsculas → GB/s).
const GPUS: &[(&str, f64)] = &[
    // NVIDIA
    ("rtx 5090", 1792.0),
    ("rtx 5080", 960.0),
    ("rtx 5070 ti", 896.0),
    ("rtx 5070", 672.0),
    ("rtx 5060 ti", 448.0),
    ("rtx 4090", 1008.0),
    ("rtx 4080", 717.0),
    ("rtx 4070 ti super", 672.0),
    ("rtx 4070 ti", 504.0),
    ("rtx 4070 super", 504.0),
    ("rtx 4070", 504.0),
    ("rtx 4060 ti", 288.0),
    ("rtx 4060", 272.0),
    ("rtx 3090 ti", 1008.0),
    ("rtx 3090", 936.0),
    ("rtx 3080 ti", 912.0),
    ("rtx 3080", 760.0),
    ("rtx 3070 ti", 608.0),
    ("rtx 3070", 448.0),
    ("rtx 3060 ti", 448.0),
    ("rtx 3060", 360.0),
    ("rtx 2080 ti", 616.0),
    ("rtx 2080", 448.0),
    ("rtx 2070", 448.0),
    ("rtx 2060", 336.0),
    ("gtx 1080 ti", 484.0),
    ("gtx 1080", 320.0),
    ("gtx 1070", 256.0),
    ("gtx 1660", 192.0),
    ("a100", 1935.0),
    ("h100", 3350.0),
    ("l40", 864.0),
    ("a6000", 768.0),
    ("a5000", 768.0),
    ("a4000", 448.0),
    ("t4", 320.0),
    ("v100", 900.0),
    // AMD
    ("rx 9070 xt", 640.0),
    ("rx 9070", 640.0),
    ("rx 7900 xtx", 960.0),
    ("rx 7900 xt", 800.0),
    ("rx 7900 gre", 576.0),
    ("rx 7800 xt", 624.0),
    ("rx 7700 xt", 432.0),
    ("rx 7600", 288.0),
    ("rx 6950 xt", 576.0),
    ("rx 6900 xt", 512.0),
    ("rx 6800", 512.0),
    ("rx 6700 xt", 384.0),
    ("rx 6600", 224.0),
    ("mi300", 5300.0),
    ("mi250", 3200.0),
    // Intel
    ("arc b580", 456.0),
    ("arc a770", 560.0),
    ("arc a750", 512.0),
    // Apple
    ("m4 max", 546.0),
    ("m4 pro", 273.0),
    ("m4", 120.0),
    ("m3 ultra", 819.0),
    ("m3 max", 400.0),
    ("m3 pro", 150.0),
    ("m3", 100.0),
    ("m2 ultra", 800.0),
    ("m2 max", 400.0),
    ("m2 pro", 200.0),
    ("m2", 100.0),
    ("m1 ultra", 800.0),
    ("m1 max", 400.0),
    ("m1 pro", 200.0),
    ("m1", 68.0),
];

/// GB/s de una GPU por nombre; `None` si es desconocida.
pub fn gpu_bandwidth(name: &str) -> Option<f64> {
    let n = name.to_lowercase();
    // La tabla está ordenada de más a menos específico dentro de cada familia.
    GPUS.iter().find(|(k, _)| n.contains(k)).map(|(_, v)| *v)
}

/// Estimación por defecto según fabricante y VRAM (para GPUs no listadas).
pub fn gpu_bandwidth_fallback(vram_bytes: u64) -> f64 {
    let gb = vram_bytes as f64 / 1e9;
    if gb >= 20.0 {
        800.0
    } else if gb >= 12.0 {
        500.0
    } else if gb >= 8.0 {
        300.0
    } else {
        200.0
    }
}

/// Ancho de banda de RAM típico para inferencia en CPU.
pub fn cpu_bandwidth(avx512: bool, physical_cores: usize) -> f64 {
    // Doble canal DDR4 ≈ 40-50 GB/s; DDR5 ≈ 60-80 GB/s. AVX-512 suele indicar plataforma reciente.
    let base: f64 = if avx512 { 70.0 } else { 45.0 };
    let base = if physical_cores >= 16 { base * 1.5 } else { base };
    // Con pocos núcleos no se llega a saturar la memoria (~8 GB/s por núcleo).
    base.min(physical_cores.max(1) as f64 * 8.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn specific_names_win() {
        assert_eq!(gpu_bandwidth("NVIDIA GeForce RTX 4070 Ti SUPER"), Some(672.0));
        assert_eq!(gpu_bandwidth("NVIDIA GeForce RTX 4070"), Some(504.0));
        assert_eq!(gpu_bandwidth("Apple M2 Pro (GPU integrada)"), Some(200.0));
        assert_eq!(gpu_bandwidth("Apple M2 (GPU integrada)"), Some(100.0));
        assert_eq!(gpu_bandwidth("AMD Radeon RX 7900 XTX"), Some(960.0));
        assert_eq!(gpu_bandwidth("GPU rara"), None);
        assert_eq!(cpu_bandwidth(false, 4), 32.0);
        assert_eq!(cpu_bandwidth(false, 8), 45.0);
    }
}
