import { getInput, getState, info, saveState } from "@actions/core";

info("main second");
info(`state ${getState("pre")}`);
info(`node ${process.version}`);
info(`name ${getInput("name")}`);
saveState("main", "saved by the main step of second");
