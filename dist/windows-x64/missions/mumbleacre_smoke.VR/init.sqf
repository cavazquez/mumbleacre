// SPDX-License-Identifier: GPL-3.0
// ACRE presets are data shared by all mission nodes, so configure them on the
// server and every client. Equipment remains fixture-local in initPlayerLocal.

[] call MumbleACRE_ACRE_fnc_configurePresets;

if (isServer) then {
    diag_log format [
        "MumbleACRE ACRE locality server: isMultiplayer=%1 isServer=%2 hasInterface=%3 isDedicated=%4 allPlayers=%5",
        isMultiplayer,
        isServer,
        hasInterface,
        isDedicated,
        count allPlayers
    ];
};
