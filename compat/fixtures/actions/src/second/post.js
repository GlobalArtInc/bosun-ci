import { getState, info } from "@actions/core";

info("post second");
info(`state ${getState("pre")}, ${getState("main")}`);
