# Guía de inicio de MRT

Un recorrido paso a paso para tu primera hora con **Modern Rust Terminal (MRT)**: instalarlo, ejecutar comandos, pedir ayuda a la IA y orientarte. Cada sección enlaza a la [User Guide](USER_GUIDE.md), la referencia completa (en inglés), para los detalles. *Prefer English? Read [Getting Started](GETTING_STARTED.md).*

La interfaz de MRT está en inglés, así que los nombres de botones y menús aparecen aquí tal como los verás en pantalla.

---

## 1. Instalación

Descarga el archivo para tu sistema desde la [última release](https://github.com/SieteCuatro/MRT/releases/latest).

**Windows (x64).** Descomprime `mrt-<versión>-x86_64-pc-windows-msvc.zip` y abre `mrt.exe`. MRT todavía no está firmado, así que la primera vez puede salir **"Windows protegió su PC"**. Elige **Más información → Ejecutar de todas formas**, o desbloquea el archivo una vez en PowerShell:

```powershell
Unblock-File .\mrt.exe
```

Si quieres escribir `mrt` desde cualquier terminal, deja `mrt.exe` en una carpeta que esté en tu `PATH`.

**Linux (x86_64).** El binario funciona en cualquier distribución con glibc 2.17 o posterior (CentOS 7, Ubuntu 14.04 y siguientes):

```bash
tar xzf mrt-<versión>-x86_64-unknown-linux-gnu.tar.gz
./mrt
# Opcional: entrada en el menú con su icono
cp mrt.desktop ~/.local/share/applications/
mkdir -p ~/.local/share/icons/hicolor/512x512/apps && cp mrt.png ~/.local/share/icons/hicolor/512x512/apps/
```

**macOS.** Todavía no hay archivos listos; [compílalo desde el código](USER_GUIDE.md#build-from-source) con `cargo build --release -p mrt-app`.

## 2. La ventana

- **Barra lateral (izquierda)**: **+ New tab** (eliges el shell), tus pestañas con un punto de estado (en espera, ejecutando, esperando una respuesta, con error) y los paneles: Explorer, Runbooks, History, MCP Hub, Toolbox, Mosaic, Notifications y **Settings**.
- **Cabecera (arriba)**: la carpeta de la pestaña, un buscador que abre la **paleta de comandos** y los botones para dividir.
- **Salida (centro)**: cada comando que ejecutas se convierte en un **bloque**.
- **Prompt (abajo)**: donde escribes. El selector de debajo cambia el shell de la pestaña (PowerShell, CMD, Bash, Zsh, WSL…).

## 3. Tus primeros comandos

Escribe un comando y pulsa **Enter**:

```
git status
```

La salida aparece en un bloque con ✔ o ✖, el código de salida y lo que tardó. Cada bloque tiene botones:

- **Copy** copia la salida y **■ Stop** detiene un comando en marcha (el shell sigue vivo). Los bloques con error tienen además **Diagnose** (una explicación de la IA) y **Report**.
- La salida que parece JSON, CSV o Markdown se muestra como árbol o como tabla que puedes filtrar.

Cada pestaña mantiene **un shell vivo**, así que `cd`, las variables y los entornos activados (`venv`, `nvm`) siguen ahí en el siguiente comando. **↑ / ↓** recorren los comandos anteriores de la pestaña.

Los comandos que empiezan por `:` son de MRT. Prueba algunos:

```
:ports              puertos abiertos y el proceso detrás de cada uno
:ps --sort mem      procesos ordenados por memoria
:http GET https://httpbin.org/json
:diff               compara los dos últimos bloques
```

La [lista completa](USER_GUIDE.md#built-in-commands) está en la User Guide.

## 4. Encuentra cualquier cosa: la paleta de comandos

Pulsa **Ctrl+K** (o Ctrl+P). La paleta lista todo lo que MRT sabe hacer: acciones con sus atajos, herramientas, vistas de procesos, ejemplos de IA, los hosts de tu `~/.ssh/config` y tus shells. Escribe unas letras y pulsa **Enter** para ejecutar la primera coincidencia. Si nada coincide, Enter ejecuta lo que escribiste como comando.

Otras búsquedas:

- **Ctrl+F** busca texto en todos los bloques de la pestaña.
- **Search history** (barra lateral) busca entre todos los comandos que has ejecutado, con su salida, de cualquier sesión.

## 5. Pregúntale a la IA

Empieza una línea con **`#`** y describe lo que quieres:

```
# encuentra los 10 archivos más grandes de esta carpeta
```

MRT escribe un comando para tu shell actual y lo muestra en una tarjeta: **Run** lo ejecuta (pasando por el Safety Gate) y **Dismiss** lo descarta. Nunca se ejecuta nada solo.

- **Sin configurar nada**, MRT usa su motor offline, que conoce las tareas habituales.
- **Con un proveedor** las respuestas son mucho mejores. Abre **Settings** (**Ctrl+,**) → **API keys** y pega una clave de Gemini, Anthropic, OpenAI, xAI, DeepSeek u OpenRouter. Se guarda en el llavero de tu sistema, no en un archivo. También puedes usar un modelo local con [Ollama](https://ollama.com) y poner **AI provider** en `ollama`.
- En un bloque con error, **Diagnose** explica el fallo y sugiere cómo arreglarlo.
- `:assist ¿por qué falló el build?` investiga con las herramientas de MRT: busca el bloque que falló, la última vez que funcionó, y los compara.
- Cada respuesta muestra los tokens que usó y un coste estimado; `:ai-usage` los suma por pestaña.

Más en [AI Providers](AI_PROVIDERS_AND_AGENTS.md).

## 6. El Safety Gate

Antes de que corra un comando arriesgado (`rm -rf`, `git push --force`, `DROP TABLE`, `kill`…), MRT muestra qué hace y por qué es arriesgado, y espera a que pulses **Run** o **Cancel**. Por defecto, los comandos críticos siempre preguntan. Puedes ajustarlo en **Settings → Safety Gate**, y `safety.blocked_patterns` en el archivo de configuración rechaza ciertos comandos directamente.

## 7. Programas a pantalla completa

`vim`, `nano`, `htop`, `less` y `lazygit` se abren en una pantalla de terminal completa sobre la ventana (cualquier otro programa, con `:tui <comando>`). El ratón y la rueda funcionan, una barra de scroll a la derecha te deja volver a lo que se salió de la pantalla, y **Ctrl+Q** termina la sesión, que se guarda como bloque. En una pestaña dividida, el programa se abre dentro de su panel.

## 8. Pestañas, divisiones y ventanas

- **Pestaña nueva**: **+ New tab** en la barra lateral, o la acción `new_tab`. Arrastra las pestañas en la barra lateral para reordenarlas.
- **Dividir**: `:split-h` (lado a lado) o `:split-v` (una sobre otra). Arrastra la línea entre los paneles para cambiar su tamaño, y `:unsplit` quita la división.
- **Otra ventana**: clic derecho en una pestaña → **Move to new window**, o escribe `:window new`. La pestaña conserva su shell y lo que esté ejecutando. `:window main` la devuelve; al cerrar una ventana, sus pestañas vuelven a la principal.
- **Mosaic** (barra lateral): todas las pestañas de la ventana a la vez, en cuadrícula.

MRT vuelve a abrir tus pestañas, divisiones y ventanas la próxima vez que arranca.

## 9. Ajústalo a tu gusto

**Settings** (**Ctrl+,**, o `:settings`) tiene un buscador y cubre la fuente, el tema, el shell por defecto, el Safety Gate, el proveedor y modelo de IA, las notificaciones y las opciones de sesión. **Save** escribe en tu archivo de configuración solo lo que cambiaste, conservando tus comentarios.

- **Temas**: `graphite-calm` (por defecto), `high-contrast`, o un archivo de tema de Windows Terminal, Alacritty, Ghostty o iTerm2.
- **Atajos de teclado**: la ventana de Settings lista todos los atajos, permite añadir nuevos (por ejemplo `ctrl+shift+t` → New tab) y avisa cuando dos chocan.
- **El archivo de configuración** es `%APPDATA%\mrt\config.toml` en Windows y `~/.config/mrt/config.toml` en Linux y macOS. Un `.mrt.toml` dentro de la carpeta de un proyecto lo sobrescribe allí. [`mrt.example.toml`](../mrt.example.toml) lista todos los ajustes; los cambios se aplican en cuanto guardas.
- **Aviso de versión nueva**: activa **Settings → Updates** para ver un aviso cuando salga una release nueva (no se descarga nada).

## 10. Red, procesos y el Toolbox

El **Toolbox** (barra lateral) tiene una pestaña **Network** (peticiones HTTP, páginas web como texto, puertos, DNS, WebSocket) y un **Task Manager**. Las mismas herramientas funcionan en el prompt:

```
:top                tabla de procesos en vivo
:proc 1234          detalles de un proceso
:kill node --tree   detiene un proceso y sus hijos (pregunta antes)
:dns github.com     registros DNS y tiempos de conexión
```

Más en [Toolbox](TOOLBOX.md).

## 11. Máquinas remotas

- `:ssh usuario@host` abre una pestaña cuyos comandos se ejecutan en esa máquina, con los mismos bloques y códigos de salida. Necesita entrar con clave SSH o agente, y no instala nada en el otro lado. Los hosts de tu `~/.ssh/config` están en la paleta, en **SSH hosts**.
- `mrt share` deja que otro ordenador o tu móvil sigan y manejen tus pestañas por una conexión cifrada; mira [Remote connections](USER_GUIDE.md#remote-connections-mrt-share-mrt-pair-mrt-attach).

## 12. Agentes de programación

MRT funciona como servidor MCP, así que agentes como Claude Code, Codex CLI y Gemini CLI pueden abrir pestañas, ejecutar comandos (pasando por el Safety Gate) y leer los resultados:

```
mrt agent setup
```

registra MRT en los agentes que encuentre (`--dry-run` muestra lo que cambiaría). Para varios agentes trabajando a la vez, `:worktree feature/login` da a cada uno su propia pestaña en un worktree de git, con botones Review, Merge y Open PR. Mira la [MCP Guide](MCP_GUIDE.md).

## 13. Cerrar MRT

Si cierras la ventana con comandos en marcha, siguen ejecutándose en segundo plano: vuelve a ejecutar `mrt` para recuperar la misma ventana, o `mrt quit` (o `:quit`) para pararlo todo. Si no hay nada en marcha, cerrar la ventana simplemente sale.

## Y ahora qué

- La [User Guide](USER_GUIDE.md): todos los comandos, atajos, ajustes y archivos, y la [solución de problemas](USER_GUIDE.md#8-troubleshooting).
- El [Changelog](../CHANGELOG.md) con lo que trajo cada release, y el [Roadmap](../ROADMAP.md) con lo que viene.
- ¿Encontraste un fallo? `:report` prepara un informe a partir del último bloque con error, sin tus claves ni secretos.
