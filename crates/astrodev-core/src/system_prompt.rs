//! Prompt de sistema: identidad, entorno, reglas y ASTRO.md del proyecto.

use crate::project::Project;
use crate::tool::ToolSpec;

pub fn build(project: &Project, tools: &[ToolSpec]) -> String {
    let mut s = String::new();
    s.push_str(
        "Eres AstroDev, un agente de programación experto que trabaja en la terminal del usuario.\n\
         Ayudas a leer, entender, modificar y depurar código del proyecto actual.\n\n\
         Reglas:\n\
         - Antes de cambiar código, explora con las herramientas (list_dir, glob, grep, read_file).\n\
         - No inventes contenido de archivos: léelos.\n\
         - Sé conciso. Responde en el idioma del usuario.\n\
         - Usa rutas relativas a la raíz del proyecto.\n\
         - Cuando termines, resume brevemente qué hiciste.\n\n",
    );
    s.push_str(&format!(
        "Entorno:\n- Raíz del proyecto: {}\n- Sistema operativo: {} ({})\n",
        project.root().display(),
        std::env::consts::OS,
        std::env::consts::ARCH
    ));
    if !tools.is_empty() {
        let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
        s.push_str(&format!(
            "- Herramientas disponibles: {}\n",
            names.join(", ")
        ));
    }
    if let Some(ctx) = project.context_file() {
        s.push_str("\nInstrucciones del proyecto (ASTRO.md):\n<astro_md>\n");
        s.push_str(ctx.trim());
        s.push_str("\n</astro_md>\n");
    }
    s
}
