//! Memoria que necesita un modelo: pesos (GGUF) + KV cache + margen de cómputo.

use serde::{Deserialize, Serialize};

/// Bits por peso efectivos de cada cuantización GGUF (incluye escalas).
pub fn bits_per_weight(quant: &str) -> f64 {
    match quant.to_uppercase().as_str() {
        "Q2_K" => 3.35,
        "Q3_K_S" => 3.5,
        "Q3_K_M" => 3.91,
        "Q4_0" => 4.55,
        "Q4_K_S" => 4.58,
        "Q4_K_M" => 4.85,
        "Q5_K_S" => 5.54,
        "Q5_K_M" => 5.69,
        "Q6_K" => 6.59,
        "Q8_0" => 8.5,
        "MXFP4" => 4.25,
        "F16" | "BF16" => 16.0,
        "F32" => 32.0,
        _ => 4.85,
    }
}

/// Factor de calidad relativo (1.0 = sin pérdida apreciable).
pub fn quality_factor(quant: &str) -> f64 {
    match quant.to_uppercase().as_str() {
        "F16" | "BF16" | "F32" => 1.0,
        "Q8_0" => 0.995,
        "Q6_K" => 0.99,
        "Q5_K_M" | "Q5_K_S" => 0.98,
        "Q4_K_M" | "MXFP4" => 0.96,
        "Q4_K_S" | "Q4_0" => 0.94,
        "Q3_K_M" | "Q3_K_S" => 0.88,
        "Q2_K" => 0.78,
        _ => 0.95,
    }
}

/// Tamaño estimado de los pesos.
pub fn weights_bytes(params_b: f64, quant: &str) -> u64 {
    (params_b * 1e9 * bits_per_weight(quant) / 8.0) as u64
}

/// KV cache: 2 (K y V) × capas × cabezas KV × dim. cabeza × contexto × bytes por valor.
/// `kv_bytes` = 2.0 para f16 (por defecto), 1.0 para q8_0.
pub fn kv_cache_bytes(layers: u32, kv_heads: u32, head_dim: u32, ctx: u32, kv_bytes: f64) -> u64 {
    (2.0 * layers as f64 * kv_heads as f64 * head_dim as f64 * ctx as f64 * kv_bytes) as u64
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct MemoryEstimate {
    pub weights: u64,
    pub kv_cache: u64,
    /// Buffers de cómputo y del runtime.
    pub overhead: u64,
    pub total: u64,
}

impl MemoryEstimate {
    pub fn new(weights: u64, kv_cache: u64) -> Self {
        // ~300 MB fijos + 6 % de los pesos para buffers de cómputo.
        let overhead = 300_000_000 + (weights as f64 * 0.06) as u64;
        Self { weights, kv_cache, overhead, total: weights + kv_cache + overhead }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn weights_match_known_sizes() {
        // Qwen2.5-Coder-7B Q4_K_M real: 4.68 GB
        let w = weights_bytes(7.62, "Q4_K_M") as f64 / 1e9;
        assert!((w - 4.62).abs() < 0.2, "{w}");
        // Llama 3.1 8B Q8_0 real: 8.54 GB
        let w = weights_bytes(8.03, "Q8_0") as f64 / 1e9;
        assert!((w - 8.53).abs() < 0.2, "{w}");
    }

    #[test]
    fn kv_cache_llama3_8b() {
        // Llama 3.1 8B: 32 capas, 8 cabezas KV, 128 dim → 128 KiB por token en f16.
        let per_token = kv_cache_bytes(32, 8, 128, 1, 2.0);
        assert_eq!(per_token, 131072);
        let ctx8k = kv_cache_bytes(32, 8, 128, 8192, 2.0);
        assert_eq!(ctx8k, 1_073_741_824); // 1 GiB
                                          // q8_0 la reduce a la mitad
        assert_eq!(kv_cache_bytes(32, 8, 128, 8192, 1.0), ctx8k / 2);
    }

    #[test]
    fn estimate_adds_overhead() {
        let e = MemoryEstimate::new(4_000_000_000, 1_000_000_000);
        assert_eq!(e.overhead, 300_000_000 + 240_000_000);
        assert_eq!(e.total, 5_540_000_000);
    }

    #[test]
    fn quant_tables() {
        assert!(bits_per_weight("q8_0") > bits_per_weight("Q4_K_M"));
        assert!(quality_factor("Q8_0") > quality_factor("Q3_K_M"));
        assert_eq!(bits_per_weight("desconocida"), 4.85);
    }
}
