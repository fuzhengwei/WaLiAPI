import { ReactNode } from "react";
import { Sidebar } from "./Sidebar";

export function Layout({ children }: { children: ReactNode }) {
  return (
    <div className="flex h-screen overflow-hidden bg-transparent text-foreground">
      <Sidebar />
      <main className="flex-1 overflow-hidden">
        <div className="h-full overflow-auto bg-[linear-gradient(180deg,rgba(255,255,255,0.02),transparent)]">
          {children}
        </div>
      </main>
    </div>
  );
}
