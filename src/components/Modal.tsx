import { useEffect } from "react";
import "./Modal.css";

export const Modal = ({
  isOpen,
  close,
  sizeFit,
  children,
  hideClose,
  zIndex,
}: {
  children: React.ReactNode;
  isOpen: boolean;
  close?: () => void;
  sizeFit?: boolean;
  hideClose?: boolean;
  zIndex?: number;
}) => {
  useEffect(() => {
    if (!isOpen || !close) {
      return;
    }

    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        event.preventDefault();
        close();
      }
    };

    window.addEventListener("keydown", onKeyDown);
    return () => {
      window.removeEventListener("keydown", onKeyDown);
    };
  }, [isOpen, close]);

  return (
    <>
      {isOpen && (
        <div
          className={`modal-container`}
          style={
            zIndex
              ? {
                  zIndex: zIndex.toString(),
                }
              : {}
          }
        >
          <div className={`modal${sizeFit ? " size-fit" : ""}`}>
            {!hideClose && close && (
              <button
                className="modal-close"
                onClick={() => {
                  close();
                }}
              >
                &#x2715;
              </button>
            )}
            <div className="modal-content">{children}</div>
          </div>
        </div>
      )}
    </>
  );
};
