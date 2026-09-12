# Validación en Arma pendiente

No se ejecutó Arma ni una sesión real de Mumble desde el entorno de desarrollo
Linux. No marcar esta lista como aprobada usando sólo los tests unitarios.

Registrar versiones de Windows, Mumble/Murmur, CBA, ACRE, hash de la DLL,
configuración de rate limit, logs del plugin y RPT de cada cliente/servidor.

Antes de la prueba entre jugadores, ejecutar `diagnose-mumbleacre.ps1`. Debe
mostrar el bundle esperado y `DLL SHA256: OK`. Después de abrir Arma debe
informar `ACRE pipe: CONNECTED`; el chat de Mumble confirma el filtrado de
audio ACRE. Es una comprobación local de instalación, pipe y contexto; no
reemplaza la prueba real de voz.

1. Dos PCs Windows x64, mismo canal administrado, misma misión y bundle;
   plugin TeamSpeak deshabilitado. Verificar handshake ACRE sin alertas.
2. Fixture `mumbleacre_smoke.VR`: directo cerca/lejos, giro de cabeza, radio
   sintonizada/no sintonizada, cambio de canal y volumen, PRC-152/PRC-117F,
   recepción múltiple y altavoz. Confirmar que no aparece voz seca de radio.
3. PTT Mumble, VAD y PTT ACRE: pulsaciones cortas, cambio directo/radio,
   soltar PTT nativo durante radio y mantenerlo luego de soltar radio.
   Medir primera voz audible y comparar con el backend oficial.
4. Vehículo con intercom, entrada/salida, Babel, muerte/inconsciencia,
   respawn, JIP, transferencia de unidad y cambios de misión.
5. Entrar desde otro canal Mumble, cerrar Arma y esperar el aviso de pipe
   desconectado. Confirmar que se libera el micrófono, se silencia estado viejo
   y el plugin regresa al canal anterior. Repetir con un usuario que ya estaba
   manualmente en `ACRE`: en ese caso debe quedarse allí.
6. Bajar intencionalmente rate limit o bloquear control: confirmar caducidad
   de TX, ausencia de micrófono pegado y recuperación con estado completo.
7. Cliente sin plugin/versión incorrecta: verificar rechazo local del control
   y documentar que un cliente stock del canal puede oír el stream crudo.
8. Listen y dedicado, luego 30 minutos con 10/30/60 participantes simulados:
   latencia p50/p95/p99, drops, CPU, memoria y contención de callbacks.

Son gates externos pendientes, junto con fidelidad acústica frente a ACRE2Core,
God/Zeus autorizado y sonidos RPC espaciales si se requieren para el release.
