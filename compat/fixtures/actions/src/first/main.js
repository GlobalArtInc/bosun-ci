import {
  addPath,
  exportVariable,
  getInput,
  getState,
  info,
  saveState,
  setOutput,
  setSecret,
} from "@actions/core";

info("main first");
info(`state ${getState("pre")}`);
const value = getInput("value");
setOutput("echo", value);
exportVariable("FIRST_VALUE", value);
addPath(getInput("directory"));
const secret = getInput("secret");
setSecret(secret);
info(`secret ${secret}`);
info(`job ${getInput("job")}`);
for (const name of [
  "GITHUB_ACTION",
  "GITHUB_ACTION_PATH",
  "GITHUB_ACTION_REPOSITORY",
  "GITHUB_ACTION_REF",
]) {
  info(`${name}=${process.env[name] ?? ""}`);
}
saveState("main", "saved by the main step of first");
