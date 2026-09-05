// SPDX-License-Identifier: GPL-3.0
// Thin mission-integration addon for ACRE2. It deliberately owns no radio UI,
// keybind, PTT, propagation, inventory item, or extension.

#include "CfgFunctions.hpp"

class CfgPatches {
    class mumbleacre_acre {
        name = "MumbleACRE ACRE mission integration";
        units[] = {};
        weapons[] = {};
        requiredVersion = 2.10;
        requiredAddons[] = {
            "cba_main",
            "acre_api",
            "acre_sys_prc152",
            "acre_sys_prc117f"
        };
        author = "MumbleACRE Team";
        version = "0.1.0";
    };
};
