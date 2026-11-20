import React, {
  createContext,
  useContext,
  useState,
  useEffect,
  useRef,
} from "react";

import { listen } from "@tauri-apps/api/event";

export interface ExtendedLogRecord {
  level: number;
  message: string;
  target?: string;
  timestamp: string;
}

export enum LogLevel {
  Trace = 1,
  Debug = 2,
  Info = 3,
  Warn = 4,
  Error = 5
}

export const LogContext = createContext<ExtendedLogRecord[]>([]);

export const LogProvider: React.FC<{ children: React.ReactNode }> = ({
  children,
}) => {
  const [logs, setLogs] = useState<ExtendedLogRecord[]>([]);
  const listenerAdded = useRef<boolean>(false);
  let unlistenRef = useRef<(() => void) | null>(null);

  useEffect(() => {
    if (!listenerAdded.current) {
      const setupLogger = async () => {
        listenerAdded.current = true;
        unlistenRef.current = await listen<ExtendedLogRecord>("log-record", (event) => {
          setLogs((prevLogs) => [...prevLogs, event.payload]);
        });
      };

      setupLogger();
    }

    return () => {
      if (unlistenRef.current) {
        unlistenRef.current();
      }
    };
  }, []);

  return (
    <LogContext.Provider value={logs}>
      {children}
    </LogContext.Provider>
  );
};

export const useLogs = () => {
  return useContext(LogContext);
};