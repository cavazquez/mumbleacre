// SPDX-License-Identifier: GPL-3.0
// Select the preset before handing out base-class ACRE radios. ACRE replaces
// them with unique IDs; setupMission then waits for that initialization before
// setting the active channel.

if (!hasInterface) exitWith {};

[] call MumbleACRE_ACRE_fnc_setupMission;

[player] execVM "giveTestRadios.sqf";

private _locality = format [
    "MumbleACRE ACRE locality host/client: isMultiplayer=%1 isServer=%2 hasInterface=%3 isDedicated=%4 allPlayers=%5",
    isMultiplayer,
    isServer,
    hasInterface,
    isDedicated,
    count allPlayers
];
diag_log _locality;
systemChat _locality;
systemChat "MumbleACRE ACRE Smoke: preparando PRC-152/PRC-117F ACRE en canal 1.";
