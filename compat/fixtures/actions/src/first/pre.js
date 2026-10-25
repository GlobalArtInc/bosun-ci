import { info, saveState } from "@actions/core";

info("pre first");
saveState("pre", "saved by the pre step of first");
