import { ReactNode } from "react";

type GlassCardProps = {
  children: ReactNode;
  className?: string;
};

export const GlassCard = ({ children, className }: GlassCardProps) => {
  const classes = ["glass-card", className].filter(Boolean).join(" ");
  return <div className={classes}>{children}</div>;
};
