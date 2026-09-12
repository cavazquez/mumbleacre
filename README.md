# MumbleACRE

La versión de distribución se toma de `Cargo.toml`. Cada corrección entregada
incrementa el último dígito de la versión, para que Mumble pueda identificar la
actualización al reinstalar el plugin.

Backend no oficial de **ACRE2 para Mumble en Arma 3**, Windows x64.
ACRE controla radios, teclas, alcance, intercom e interfaces; Mumble transporta
la voz. No necesita TFAR, un bridge ni una extensión Arma propia.

**Estado: candidato para pruebas.** El runtime está conectado y se verifica
con pruebas automatizadas y compilación Windows. Falta validar una partida
con dos clientes Windows y Arma.

## Instalar y usar

1. Usar Mumble **1.5.634 o posterior de la rama 1.5**, x64, y ACRE2
   **2.14.0.1064** + CBA_A3 en Arma. Otras versiones de ACRE requieren revisar
   su protocolo privado antes de declararlas compatibles.
2. Instalar `dist/windows-x64/MumbleACRE.mumble_plugin` desde las opciones de
   plugins de Mumble. Cerrar cualquier otro cliente que esté usando ACRE antes
   de abrir Mumble: los pipes de ACRE sólo pueden tener un propietario.
3. En el servidor Mumble configurar y reiniciar:

   ```ini
   pluginmessagelimit=20
   pluginmessageburst=40
   ```

   Estos límites se comparten entre los plugins de cada cliente. El valor por
   defecto no alcanza para este backend. Ver [configuración oficial de Mumble](https://github.com/mumble-voip/mumble/blob/master/auxiliary_files/mumble-server.ini).
4. Crear en el servidor Mumble un canal llamado exactamente `ACRE` (en
   mayúsculas) y dar a los jugadores permiso para entrar. Al conectarse ACRE,
   el plugin busca ese nombre con coincidencia exacta —`acre`, `ACRE 1` y
   cualquier nombre que sólo lo contenga no sirven— y solicita que Mumble mueva al usuario local. Al cerrar
   Arma o perder el pipe ACRE, vuelve al canal en el que estaba antes de ese
   cambio automático. `diagnose-mumbleacre.ps1` registra y muestra la última
   acción de canal. No usa contraseña de canal ni cambia grupos. El plugin usa el canal Mumble activo
   como límite de la partida, así que no requiere un identificador de misión:

   ```powershell
   .\scripts\start-mumble.ps1
   ```

   `MUMBLEACRE_MISSION` queda disponible como override opcional para aislar un
   grupo adicionalmente; si se usa, debe tener el mismo valor en todos los
   clientes. Habilitar MumbleACRE en la lista de plugins si todavía no está
   habilitado.
5. Confirmar que todos quedaron en `ACRE`, el mismo canal Mumble
   **administrado y exclusivo de esa partida**, con el mismo plugin. Si el
   cambio falla, comprobar primero que exista ese nombre exacto y que el
   usuario pueda entrar. El servidor dedicado de Arma sólo necesita CBA + ACRE
   y los mods de la misión. El backend se instala en cada cliente.
6. Para la fixture, cargar también `@mumbleacre` y copiar
   `missions/mumbleacre_smoke.VR` a las misiones de Arma. Tras iniciar, la
   misión confirma en el chat que entregó una PRC-152 en C1 (y una PRC-117F en
   la mochila); si no lo hace, ACRE2 no está cargado correctamente. El addon
   sólo agrega presets de misión y es opcional para misiones ACRE existentes.

La voz directa usa el PTT/VAD de Mumble; radio e intercom usan las teclas de
ACRE. Fuera de una sesión ACRE conectada, el plugin deja pasar el audio normal
de Mumble; deshabilitarlo sólo evita el cambio automático a `ACRE` al iniciar
Arma. No usar whisper/shout ni canales enlazados para la partida. Un usuario
sin plugin en el canal puede oír la voz transportada sin los filtros ACRE;
el canal debe excluir esos clientes. Esto no es cifrado de redes de radio.
El nombre visible en Mumble sigue siendo el configurado por el usuario: la API
de plugins usada por Mumble no expone una operación para renombrar usuarios.

## Qué incluye

- Pipes ACRE, handshake, identidad real de sesión Mumble y metadatos VOIP.
- Estado completo entre pares, reconexión, secuencias y vencimiento de 250 ms.
- PTT ACRE conectado al micrófono; voz directa nativa y cambios de modo.
- Render de decisiones ACRE: distancia, paneo, radio multipath, altavoz,
  intercom y Babel. Sin decisión vigente se silencia la fuente.
- Sonidos locales centrados de ACRE, preparados fuera del callback de audio;
  pips genéricos de radio de respaldo si una partida ya iniciada no reenvía sus
  sonidos después de reconectar el plugin.
- PRC-152 y PRC-117F con presets de misión y fixture de dos jugadores.

Los modos remotos God/Zeus se rechazan hasta contar con autorización adicional;
los renderers de espectador/God sólo procesan decisiones emitidas por ACRE
para una transmisión admitida. Los sonidos mundiales/posicionales solicitados
por RPC no están soportados por `playSample` API 1.0 y devuelven error a ACRE.
La latencia del primer audio, solapamiento PTT/VAD, fidelidad DSP y comportamiento
bajo carga siguen pendientes de medición en Mumble/Arma reales. El control
puede silenciar el comienzo de una transmisión mientras llegan las decisiones.

## Compilar y verificar

Ubuntu/WSL: Rust 1.91, `mingw-w64`, Python 3.11+, `armake2`, `rg` y `unzip`.

```bash
rustup target add x86_64-pc-windows-gnu
cargo install armake2 --version 0.3.0 --locked
./check.sh
./scripts/build-windows.sh
```

El build genera DLL, bundle nativo de Mumble, PBO, fixture y `SHA256SUMS` en
`dist/windows-x64/`. No instala nada automáticamente. Linux sirve para probar
contratos y DSP; el plugin requiere Windows para conectar los pipes ACRE.
El bundle usa el [formato nativo de Mumble](https://github.com/mumble-voip/mumble/blob/master/docs/dev/plugins/Bundling.md).

`check.sh` ejecuta formato, Clippy, pruebas Rust, contratos SQF y compilación
Windows. SQF-VM se descarga con hash fijado; se puede pasar `SQFVM_BIN` para
usar una copia local. CI también ejecuta las pruebas nativas Windows.

Diagnóstico del pipe: `%LOCALAPPDATA%\MumbleACRE\logs\plugin.log`, con rotación.
El chat/log Mumble informa arranque, conexión ACRE, solicitud de canal, cambio
entre audio normal y filtrado ACRE, pérdida de salud y colas saturadas. Si no
conecta: revisar que MumbleACRE esté habilitado, la versión de ACRE, que ningún
otro cliente posea el pipe y el canal Mumble. Si se corta la voz: revisar
primero los límites del servidor y luego guardar logs y RPT para la prueba de
QA.

Para reunir un reporte local sin cambiar la configuración ni necesitar otro
jugador, ejecutar:

```powershell
.\scripts\diagnose-mumbleacre.ps1
```

El script comprueba el bundle, hash de la DLL distribuida, instalación de
Mumble, proceso activo, el estado del pipe ACRE para ese proceso y las últimas
líneas del log. Si `murmur.ini` está en esa misma máquina, agrega
`-MurmurIni C:\ruta\murmur.ini` para comprobar los dos límites requeridos. La
copia distribuida incluye el mismo script junto a `start-mumble.ps1`.

[Arquitectura y límites](docs/runtime.md) · [Misión](docs/acre-mission-integration.md)
· [QA pendiente](docs/qa.md) · [Proveniencia y GPL-3.0](NOTICE.md)
