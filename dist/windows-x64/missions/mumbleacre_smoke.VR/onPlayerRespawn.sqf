// SPDX-License-Identifier: GPL-3.0
// Re-run only the public ACRE setup for the new local unit. The function has a
// deadline, so a failed ACRE conversion produces one diagnostic instead of a
// persistent script error.

params ["_newUnit"];

if (!hasInterface) exitWith {};

[] call MumbleACRE_ACRE_fnc_setupMission;

_newUnit addItem "ACRE_PRC152";
if ((backpack _newUnit) isEqualTo "") then {
    _newUnit addBackpack "B_AssaultPack_khk";
};
_newUnit addItemToBackpack "ACRE_PRC117F";
