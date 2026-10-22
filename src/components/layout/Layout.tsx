import { ReactNode } from "react";
import { Sidebar } from "./Sidebar";

export function Layout({
  children,
  hasUpdate,
  onCheckUpdate,
}: {
  children: ReactNode;
  hasUpdate: boolean;
  onCheckUpdate: () => void;
}) {
  return (
    <div className="flex h-screen overflow-hidden bg-transparent text-foreground">
      <Sidebar hasUpdate={hasUpdate} onCheckUpdate={onCheckUpdate} />
      <main className="flex-1 overflow-hidden">
        <div className="h-full overflow-auto bg-[linear-gradient(180deg,rgba(255,255,255,0.02),transparent)]" data-wali-scroll>
          {children}
        </div>
      </main>
    </div>
  );
}
