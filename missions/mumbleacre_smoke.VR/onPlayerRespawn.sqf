// SPDX-License-Identifier: GPL-3.0
// Re-run only the public ACRE setup for the new local unit. The function has a
// deadline, so a failed ACRE conversion produces one diagnostic instead of a
// persistent script error.

params ["_newUnit"];

if (!hasInterface) exitWith {};

[] call MumbleACRE_ACRE_fnc_setupMission;

[_newUnit] execVM "giveTestRadios.sqf";
