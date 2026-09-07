# Runtime MumbleACRE

```text
ACRE2Arma -> named pipes -> worker ACRE -> control Windows -> sendData
                              |               |                |
                          decisiones      micrófono        otros clientes
                              |                                |
                         snapshot RT <--- ACRE <- remoteStart/StopSpeaking
                              |
                      callback audio Mumble
```

El timer Windows del hilo principal procesa control cada 20 ms. Ese hilo llama
la API Mumble; los workers de pipes y sonidos nunca la llaman. Shutdown detiene
el timer y espera esos workers, que no dependen del hilo principal para salir.
Las colas tienen capacidad fija. Una saturación de eventos/acciones fuerza
reset en lugar de conservar un PTT cuyo STOP se pudo perder.

`MumbleACRE:STATE:1` envía un JSON de hasta 1024 bytes: header de versión,
misión, generación y secuencia, versión ACRE, netId y transmisión activa o null.
Cada mensaje es completo: no depende de recibir un HELLO/START anterior. Se
refresca a 10 Hz y las transiciones se coalescen hasta un máximo de 20 Hz.
El receptor rechaza versión/scope/identidad inválidos y secuencias antiguas;
conserva la última secuencia tras vencer una transmisión, de modo que un replay
no la reabra. Un estado nuevo puede reabrirla legítimamente.

Mumble aporta el ID de sesión autoritativo y la lista de miembros del canal;
un payload no puede escoger otro ID de voz. Cambiar servidor/canal elimina el
estado anterior. El scope por defecto es `mumble-channel` y sólo se aceptan
mensajes de miembros del canal Mumble actual. `MUMBLEACRE_MISSION` es un override
opcional compartido, no una credencial ni una identidad de misión autenticada por
Arma. El protocolo asume miembros confiables del canal; no autoriza God/Zeus por
un campo enviado por un peer.

Cuando ACRE se conecta, ese mismo timer busca el canal llamado exactamente
`ACRE` mediante la API de Mumble (la búsqueda distingue mayúsculas/minúsculas)
y solicita mover al usuario local. No usa contraseñas, grupos ni coincidencias
parciales. Sólo hay un intento por conexión/usuario/servidor Mumble: si el canal
no existe o el servidor rechaza la entrada, se registra el error sin reintentar
en un bucle.

Una transmisión remota vence tras 250 ms sin estado nuevo, emite STOP y pierde
el permiso de audio. La callback verifica ese mismo plazo con reloj monotónico
incluso si el hilo principal está detenido. ACRE decide si se oye y cómo: nunca
se calcula disponibilidad de radios a partir de frecuencias en el plugin.
Los mensajes de control no están vinculados criptográficamente a cada paquete
Opus; no se afirma ausencia absoluta de carreras entre control y voz bajo
latencias arbitrarias. La prueba de transición directo/radio es obligatoria.

La entrada PCM16 también tiene un permiso de 250 ms renovado sólo después de
un envío de estado exitoso. Si se pierde control, deja de enviar voz útil.
El callback de entrada informa voz nativa mediante atómicos y distingue el
micrófono forzado de ACRE; esto permite volver a PTT/VAD sin depender sólo de
un callback de cambio de habla. El inicio puede recortarse: no hay pre-roll de
voz aprobado todavía. `sendData` exitoso no confirma recepción, por eso el
límite del servidor y las mediciones de QA siguen siendo requisitos.

Los RPC `setSoundSystemMasterOverride` y `localMute` pasan por el control del
hilo principal: el primero silencia el render de voz de ACRE y el segundo
desactiva el micrófono local. `setMuted` solicita el mute local de Mumble para
el usuario indicado. Ninguna de esas operaciones se ejecuta desde el worker de
pipes ni el callback de audio.

El filtro ACRE sólo se activa cuando el pipe está conectado y existe un
contexto Mumble sincronizado. Antes de eso —o ante pérdida del pipe, del
contexto o un reset— la callback declara que no modificó el PCM: Mumble conserva
su audio de voz normal. Una vez activa la sesión, una fuente sin decisión ACRE
vigente sí se silencia como parte de la política ACRE.

El chat de Mumble y `%LOCALAPPDATA%\MumbleACRE\logs\plugin.log` registran cada
transición entre `audio normal de Mumble activo` y `filtrado de audio ACRE
activo`. Es una comprobación local: no requiere otro jugador y permite separar
un problema de pipe/contexto de uno de transmisión entre pares.

Audio: 128 slots DSP preasignados, acceso exclusivo con atómico sin espera,
snapshots inmutables y búsqueda por sesión. Colisiones de slot reinician DSP;
un callback concurrente sobre un slot ocupado se silencia. No se llama a la
API, no se hace I/O y no se toma el mutex de control desde el audio. Las pruebas
cubren asignaciones del renderer; la publicación retiene snapshots retirados hasta que el control puede liberarlos
fuera del callback. Falta medir contención y carga real antes del release.

Soportados en código: directo, radio, intercom, Babel, multipath y posición de
altavoces en decisiones de radio. God/Zeus remotos se rechazan; `setSetting` y
`setTs3ChannelDetails` se validan pero no cambian configuración del cliente.
Sonidos RPC mundiales/posicionales no se reproducen. Los tests de codecs no
constituyen prueba de paridad funcional de toda la superficie ACRE.

Los sonidos cargados por ACRE se conservan entre reconexiones del pipe: ACRE
puede recordar una carga durante toda la sesión de Arma y no volver a enviar el
WAV al reconectar el plugin. Si aun así pide `Acre_GenericBeep` o los clicks
genéricos sin reenviar su archivo, el worker genera un pip local centrado. Una
carga posterior de ACRE reemplaza ese respaldo.
