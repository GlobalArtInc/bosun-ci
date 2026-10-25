import { info, saveState } from "@actions/core";

info("pre second");
saveState("pre", "saved by the pre step of second");
