import { getState, info } from "@actions/core";

info("post first");
info(`state ${getState("pre")}, ${getState("main")}`);
