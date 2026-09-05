class CfgFunctions {
    class MumbleACRE_ACRE {
        class Mission {
            file = "\mumbleacre\addons\mumbleacre_acre\functions";

            // Define all project presets on every ACRE node before radios are
            // handed to players. Does not select a preset for a player.
            class configurePresets {};

            // Select the local side preset, then wait for ACRE's unique radio
            // IDs before selecting the requested active channel.
            class setupMission {};
        };
    };
};
