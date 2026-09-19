package dev.sterne.quickjsagent

import android.graphics.Color
import android.graphics.Typeface
import android.os.Bundle
import android.text.method.ScrollingMovementMethod
import android.view.Gravity
import android.view.ViewGroup
import android.widget.Button
import android.widget.CheckBox
import android.widget.EditText
import android.widget.LinearLayout
import android.widget.ScrollView
import android.widget.TextView
import androidx.appcompat.app.AppCompatActivity
import java.io.File
import java.io.InputStream
import java.io.OutputStream

/**
 * QuickJS agent 的最小壳 —— 不是产品 UI，是把 spike 二进制在手机上跑起来的外壳。
 *
 * 为什么这么做：spike 是个 CLI（`spikes/quickjs-agent`），仓库里现有的 App 是
 * **A 路线（Tauri + bun bundle）**，跟 QuickJS 无关。所以这里做的事只有三件：
 *   1. 把 spike 二进制以 `lib*.so` 的身份塞进 jniLibs，让系统把它解包到
 *      nativeLibraryDir（**只有那里可执行** —— Android 10+ 禁止从 data 目录 exec）；
 *   2. 起进程、把 stdout/stderr 显示出来；
 *   3. 因为没有终端，把审批提示变成按钮：按钮往 stdin 写 y/n/a。
 *
 * 也就是说：路线本身（QuickJS 宿主 + Rust 工具链 + 会话/审批/MCP）一行没改，
 * 这个壳只负责「怎么在手机上按下去」。
 */
class MainActivity : AppCompatActivity() {

    private lateinit var output: TextView
    private lateinit var scroll: ScrollView
    private lateinit var promptInput: EditText
    private lateinit var keyInput: EditText
    private lateinit var resumeBox: CheckBox
    private lateinit var runButton: Button
    private lateinit var stopButton: Button

    private var process: Process? = null
    private var stdin: OutputStream? = null

    private val prefs by lazy { getSharedPreferences("quickjs-agent", MODE_PRIVATE) }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContentView(buildUi())

        keyInput.setText(prefs.getString("api_key", ""))
        promptInput.setText(prefs.getString("last_prompt", "列出工作区里的文件，然后说一句话总结。"))
        append(
            "QuickJS agent spike —— 壳会执行 nativeLibraryDir 里的 libquickjsagent.so\n" +
                "首次使用：填 API key → 点「运行」。审批会在这里弹提示，用按钮回答。\n\n",
        )
    }

    // ── UI（用代码搭，省掉 layout XML）──────────────────────────────────
    private fun buildUi(): ViewGroup {
        val root = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            setPadding(24, 32, 24, 24)
        }

        root.addView(label("API key（存本机 SharedPreferences，仅传给它自己的进程）"))
        keyInput = EditText(this).apply {
            hint = "sk-…"
            inputType = android.text.InputType.TYPE_CLASS_TEXT or
                android.text.InputType.TYPE_TEXT_VARIATION_PASSWORD
            setTextColor(Color.WHITE)
        }
        root.addView(keyInput)

        root.addView(label("任务"))
        promptInput = EditText(this).apply {
            setTextColor(Color.WHITE)
            maxLines = 3
        }
        root.addView(promptInput)

        resumeBox = CheckBox(this).apply {
            text = "继续上次会话（--resume）"
            isChecked = true
            setTextColor(Color.LTGRAY)
        }
        root.addView(resumeBox)

        val row = LinearLayout(this).apply { orientation = LinearLayout.HORIZONTAL }
        runButton = Button(this).apply {
            text = "运行"
            setOnClickListener { startRun() }
        }
        stopButton = Button(this).apply {
            text = "停止"
            isEnabled = false
            setOnClickListener { stopRun() }
        }
        row.addView(runButton, LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f))
        row.addView(stopButton, LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f))
        root.addView(row)

        root.addView(label("审批（终端里本来是 stdin，这里换成按钮）"))
        val approvals = LinearLayout(this).apply { orientation = LinearLayout.HORIZONTAL }
        approvals.addView(answerButton("允许", "y\n"))
        approvals.addView(answerButton("拒绝", "n\n"))
        approvals.addView(answerButton("总是", "a\n"))
        approvals.addView(answerButton("全拒", "d\n"))
        root.addView(approvals)

        val misc = LinearLayout(this).apply { orientation = LinearLayout.HORIZONTAL }
        misc.addView(Button(this).apply {
            text = "清屏"
            setOnClickListener { output.text = "" }
        })
        misc.addView(Button(this).apply {
            text = "保存 key"
            setOnClickListener {
                prefs.edit().putString("api_key", keyInput.text.toString()).apply()
                append("[壳] API key 已保存\n")
            }
        })
        root.addView(misc)

        scroll = ScrollView(this)
        output = TextView(this).apply {
            typeface = Typeface.MONOSPACE
            setTextColor(Color.parseColor("#D0D6DC"))
            textSize = 11f
            movementMethod = ScrollingMovementMethod()
            setTextIsSelectable(true)
        }
        scroll.addView(output)
        root.addView(scroll, LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, 0, 1f))
        return root
    }

    private fun label(text: String) = TextView(this).apply {
        this.text = text
        setTextColor(Color.GRAY)
        textSize = 12f
        setPadding(0, 16, 0, 4)
    }

    private fun answerButton(text: String, payload: String) = Button(this).apply {
        this.text = text
        setOnClickListener {
            val pipe = stdin
            if (pipe == null) {
                append("[壳] 现在没有在等输入\n")
                return@setOnClickListener
            }
            runCatching {
                pipe.write(payload.toByteArray())
                pipe.flush()
                append("[壳] → $text\n")
            }.onFailure { append("[壳] 写 stdin 失败: ${it.message}\n") }
        }
    }

    // ── 进程 ────────────────────────────────────────────────────────────
    private fun startRun() {
        if (process?.isAlive == true) {
            append("[壳] 还在跑，先「停止」\n")
            return
        }
        val key = keyInput.text.toString().trim()
        if (key.isEmpty()) {
            append("[壳] 先填 API key（或者用宿主 mock：见 README）\n")
            return
        }
        prefs.edit()
            .putString("api_key", key)
            .putString("last_prompt", promptInput.text.toString())
            .apply()

        val exe = File(applicationInfo.nativeLibraryDir, "libquickjsagent.so")
        if (!exe.canExecute()) {
            append("[壳] 找不到可执行的 ${exe.absolutePath}（canExecute=false）\n")
            return
        }

        val workspace = File(filesDir, "workspace").apply { mkdirs() }
        val data = File(filesDir, "data").apply { mkdirs() }
        val args = mutableListOf(
            exe.absolutePath,
            "--workspace", workspace.absolutePath,
            "--data-dir", data.absolutePath,
            "--prompt", promptInput.text.toString(),
        )
        if (resumeBox.isChecked) args.add("--resume")

        append("\n[壳] ${args.joinToString(" ")}\n\n")

        val builder = ProcessBuilder(args)
        builder.environment()["DEEPSEEK_API_KEY"] = key
        // 别把壳的环境整个透传（HOME 等指向 Android 的怪路径）
        builder.environment().remove("LD_PRELOAD")

        val started = runCatching { builder.start() }
        if (started.isFailure) {
            append("[壳] 起进程失败: ${started.exceptionOrNull()?.message}\n")
            return
        }
        val proc = started.getOrThrow()
        process = proc
        stdin = proc.outputStream
        runButton.isEnabled = false
        stopButton.isEnabled = true

        pump(proc.inputStream)
        pump(proc.errorStream)
        Thread {
            val code = proc.waitFor()
            runOnUiThread {
                append("\n[壳] 进程退出，code=$code\n")
                runButton.isEnabled = true
                stopButton.isEnabled = false
                stdin = null
                process = null
            }
        }.start()
    }

    private fun stopRun() {
        process?.let {
            append("[壳] destroy()\n")
            it.destroy()
        }
    }

    /** 逐块搬输出（审批提示是没有换行的，按行读会卡住）。 */
    private fun pump(stream: InputStream) {
        Thread {
            val buffer = ByteArray(2048)
            try {
                while (true) {
                    val read = stream.read(buffer)
                    if (read <= 0) break
                    val text = String(buffer, 0, read)
                    runOnUiThread { append(text) }
                }
            } catch (_: Exception) {
                // 进程结束时的正常现象
            }
        }.start()
    }

    private fun append(text: String) {
        output.append(text)
        scroll.post { scroll.fullScroll(ScrollView.FOCUS_DOWN) }
    }
}
