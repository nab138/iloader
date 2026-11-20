import React, { createContext, useContext, useEffect, useState } from "react";
import "./DialogContext.css";

export const PlatformContext = createContext<{
  platform: "windows" | "mac" | "linux";
}>({ platform: "windows" });

export const PlatformProvider: React.FC<{ children: React.ReactNode }> = ({
  children,
}) => {
  const [platform, setPlatform] = useState<"mac" | "windows" | "linux">(
    "windows",
  );

  useEffect(() => {
    if (typeof navigator === "undefined") return;
    const ua = navigator.userAgent || "";
    if (ua.includes("Mac")) {
      setPlatform("mac");
    } else if (ua.includes("Win")) {
      setPlatform("windows");
    } else if (ua.includes("Linux")) {
      setPlatform("linux");
    }
  }, []);

  return (
    <PlatformContext.Provider
      value={{
        platform,
      }}
    >
      {children}
    </PlatformContext.Provider>
  );
};

export const usePlatform = () => {
  return useContext(PlatformContext);
};
