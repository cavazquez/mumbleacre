# Contrato RPC ACRE2 `v2.14.0.1064`

El codec y la integración de control están implementados; la validación E2E
pendiente está en [QA](qa.md).

La frontera es privada de ACRE2 y usa texto ASCII:

```text
procedimiento:parametro1,parametro2,...
```

Los mensajes pueden llegar sin NUL desde `ACRE2Arma` o con un NUL final desde el
backend de voz. El codec acepta ambas formas, rechaza bytes no ASCII, bytes
después de un NUL, procedimientos inválidos, más de 1024 parámetros y frames de
más de 4096 bytes.

El corpus está en
[`crates/mumbleacre-acre/tests/fixtures/acre2-v2.14.0.1064-rpc.txt`](../crates/mumbleacre-acre/tests/fixtures/acre2-v2.14.0.1064-rpc.txt).
Las pruebas lo compilan con `include_str!`: cambiar el fixture sin mantener su
compatibilidad rompe el gate.

## Estado por familia

| Familia | Implementación |
|---|---|
| Salud/identidad | `ping`, versión, `getClientID`, reset; identidad Mumble sincronizada. |
| Estado local | Pose, idioma y curva; `setSetting` se valida sin cambiar Mumble. |
| Control de cliente | `setSoundSystemMasterOverride`, `localMute`, `setMuted` y `setPTTKeys`; aplica silencios de ACRE sin romper el pipe. |
| VOIP | Nombre/UID de servidor y canal; `setTs3ChannelDetails` es no-op validado. |
| PTT | Radio/intercom y voz nativa conectados; God/Zeus sólo codec, no envío remoto. |
| Pares | Estado completo Mumble convertido a `remoteStartSpeaking`/`remoteStopSpeaking`. |
| Audio | `updateSpeakingData` tipado, publicación inmutable y DSP ACRE. |
| Sonidos | `loadSound`/`playLoadedSound`, sólo locales centrados; pips genéricos de respaldo si ACRE no reenvía un sonido ya recordado. Otros reciben error. |

En `v2.14.0.1064`, `getClientID` llega desde Arma con dos parámetros:
`netId` y `playerUID`. MumbleACRE conserva el `netId` para asociar el estado
de voz y devuelve el ID de usuario de Mumble junto con ese mismo `netId`.

`setSoundSystemMasterOverride` silencia el audio de voz mientras ACRE mantiene
el override (por ejemplo, durante briefing). `localMute` bloquea el micrófono
local y `setMuted` usa el mute local por usuario de Mumble. `setPTTKeys` se
valida como no-op: el handler original de ACRE2 también tiene su efecto de
teclas deshabilitado y el PTT se controla mediante los RPC de inicio/parada.

Un procedimiento desconocido se rechaza; no se intenta una compatibilidad
silenciosa con otro release de ACRE.

## `updateSpeakingData`

El parser `parse_update_speaking_data` valida aridad, tipos y floats finitos y
produce una decisión pura:

| Tipo ACRE | Forma | Resultado tipado |
|---|---|---|
| `d` | directo | volumen, posición y vector de cabeza |
| `i` | intercom | igual a directo, marcado intercom |
| `z` | Zeus | igual a directo, marcado Zeus |
| `r` | radio | `N` caminos: volumen, señal, modelo, altavoz y posición |
| `m` | mute | decisión de silencio |
| `s` | espectador | volumen de espectador |
| `g` | God | volumen God |

Las coordenadas se conservan como `x,z,y`, el mismo orden usado por el mixer
ACRE. El worker coalesce la última decisión validada por sesión y el control
plane la publica como snapshot inmutable, con generación, secuencia y timestamp
de vigencia. El callback consume los renderers ya implementados sin
volver a calcular frecuencias, alcance, targets o disponibilidad de radio; el
control peer solicita esas decisiones por el pipe de ACRE.

## Actualizar ACRE

Para soportar otro release:

1. fijar el tag/versión exactos;
2. comparar `ACRE2Core/Engine.cpp`, `TextMessage.*` y todos los handlers;
3. actualizar el fixture y el inventario juntos;
4. ampliar los parsers tipados necesarios;
5. ejecutar el gate Rust y la prueba Windows de QA antes de declarar soporte.
