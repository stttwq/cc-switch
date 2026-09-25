import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import "@testing-library/jest-dom";
import { TerminalSettings } from "@/components/settings/TerminalSettings";

vi.mock("react-i18next", () => ({
  useTranslation: () => ({ t: (key: string) => key }),
}));

vi.mock("@/lib/platform", () => ({
  isMac: () => false,
  isWindows: () => true,
  isLinux: () => false,
}));

vi.mock("@/components/ui/input", () => ({
  Input: (props: React.InputHTMLAttributes<HTMLInputElement>) => <input {...props} />,
}));

vi.mock("@/components/ui/select", () => ({
  Select: ({ children }: { children: React.ReactNode }) => <div>{children}</div>,
  SelectTrigger: ({ children }: { children: React.ReactNode }) => <div>{children}</div>,
  SelectValue: () => null,
  SelectContent: ({ children }: { children: React.ReactNode }) => <div>{children}</div>,
  SelectItem: ({ children }: { children: React.ReactNode }) => <div>{children}</div>,
}));

describe("TerminalSettings", () => {
  it("syncs an asynchronously loaded custom path without saving an empty draft on blur", () => {
    const onCustomChange = vi.fn();
    const props = {
      value: "custom",
      onChange: vi.fn(),
      onCustomChange,
    };
    const { rerender } = render(<TerminalSettings {...props} />);
    const pathInput = screen.getByPlaceholderText(
      "settings.terminal.customPathPlaceholder",
    );

    rerender(
      <TerminalSettings
        {...props}
        customPath={String.raw`C:\Users\jia\AppData\Local\Programs\Pebrel\pebrel.exe`}
      />,
    );

    expect(pathInput).toHaveValue(
      String.raw`C:\Users\jia\AppData\Local\Programs\Pebrel\pebrel.exe`,
    );
    fireEvent.blur(pathInput);
    expect(onCustomChange).not.toHaveBeenCalled();
  });

  it("saves a custom path only after the user edits it", () => {
    const onCustomChange = vi.fn();
    render(
      <TerminalSettings
        value="custom"
        customPath={String.raw`C:\Pebrel\pebrel.exe`}
        onChange={vi.fn()}
        onCustomChange={onCustomChange}
      />,
    );
    const pathInput = screen.getByPlaceholderText(
      "settings.terminal.customPathPlaceholder",
    );

    fireEvent.change(pathInput, {
      target: { value: String.raw`C:\New\terminal.exe` },
    });
    fireEvent.blur(pathInput);

    expect(onCustomChange).toHaveBeenCalledWith({
      preferredTerminalCustomPath: String.raw`C:\New\terminal.exe`,
    });
  });
});
