import { AppError } from "../errors";

export type Operation = {
  id: string;
  titleKey: string;
  successMessageKey?: string;
  successTitleKey?: string;
  steps: OperationStep[];
};

export type OperationStep = {
  id: string;
  titleKey: string;
};

export type OperationState = {
  current: Operation;
  completed: string[];
  started: string[];
  failed: {
    stepId: string;
    extraDetails: AppError;
  }[];
};

type OperationInfoUpdate = {
  updateType: "started" | "finished";
  stepId: string;
};

type OperationFailedUpdate = {
  updateType: "failed";
  stepId: string;
  extraDetails: AppError;
};

export type OperationUpdate = OperationInfoUpdate | OperationFailedUpdate;

export const installSideStoreOperation: Operation = {
  id: "install_sidestore",
  titleKey: "operations.install_sidestore_title",
  successTitleKey: "operations.install_sidestore_success_title",
  successMessageKey: "operations.install_sidestore_success_message",
  steps: [
    {
      id: "download",
      titleKey: "operations.install_sidestore_step_download",
    },
    {
      id: "install",
      titleKey: "operations.install_sidestore_step_install",
    },
    {
      id: "pairing",
      titleKey: "operations.install_sidestore_step_pairing",
    },
  ],
};

export const installLiveContainerOperation: Operation = {
  id: "install_sidestore",
  titleKey: "operations.install_livecontainer_title",
  successTitleKey: "operations.install_livecontainer_success_title",
  successMessageKey: "operations.install_livecontainer_success_message",
  steps: [
    {
      id: "download",
      titleKey: "operations.install_livecontainer_step_download",
    },
    {
      id: "install",
      titleKey: "operations.install_livecontainer_step_install",
    },
    {
      id: "pairing",
      titleKey: "operations.install_livecontainer_step_pairing",
    },
  ],
};

export const sideloadOperation = {
  id: "sideload",
  titleKey: "operations.sideload_title",
  steps: [
    {
      id: "install",
      titleKey: "operations.sideload_step_install",
    },
  ],
};
