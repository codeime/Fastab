import { GetPlatformInfoResponse } from "@fastab/proto/fig";
import { sendGetPlatformInfoRequest } from "./requests.js";
import {
  AppBundleType,
  DesktopEnvironment,
  DisplayServerProtocol,
  Os,
} from "@fastab/proto/fig";

export { AppBundleType, DesktopEnvironment, DisplayServerProtocol, Os };

export function getPlatformInfo(): Promise<GetPlatformInfoResponse> {
  return sendGetPlatformInfoRequest({});
}
