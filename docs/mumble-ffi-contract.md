# ABI Mumble

Se solicita API 1.0 para Mumble 1.5. El binding copia punteros de función de la
tabla temporal de Mumble; no conserva su dirección. Los offsets de `sendData`
(35), `playSample` (37), controles y tamaño se verifican en tests.

Las consultas de conexión sincronizada, ID local, canal, miembros, hash de
servidor y nombre de canal comprueban el resultado antes de leer sus salidas.
Las listas/strings se liberan con `freeMemory`. Los destinatarios se restringen
al canal local y la capacidad del backend es de 128 miembros.

El control llama Mumble desde su hilo principal mediante un timer Windows;
los callbacks de audio usan sólo estado atómico. Los workers de pipe y sonido
no hacen llamadas Mumble. Esto evita esperar un worker que a su vez espere al
hilo principal. Véase la [API oficial](https://github.com/mumble-voip/mumble/blob/master/docs/dev/plugins/MumbleAPI.md).

La entrada PCM16 publica la detección de habla sin mutex. La salida PCM float
sólo modifica fuentes speech; notificaciones y música quedan intactas.
`playSample` API 1.0 no lleva posición: sólo se aceptan sonidos locales centrados.
