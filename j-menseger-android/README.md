# J-Menseger para Android

Página (HTML, CSS e JavaScript) dentro de uma WebView, com uma camada Android
fina em Java para o que a página não faz sozinha: guardar a chave no Android
Keystore, notificações, chamada em tela cheia, conexão em segundo plano, viva-voz.

## De onde veio este código

O código-fonte original do celular (versão 0.4.0-beta) se perdeu junto com o
ambiente onde foi escrito. Esta pasta foi **reconstruída a partir do APK
0.4.0-beta**:

- `web/index.html`, `web/styles.css`, os sons e as fontes: tirados do APK
  (o CSS foi só reformatado).
- `web/app.js`: o JavaScript do APK, reformatado para leitura. Nomes de
  métodos, textos e HTML estão como eram; os nomes de variáveis internas são
  os curtos que o empacotador gerou. As bibliotecas (nostr-tools, @noble,
  @scure) estão no começo do arquivo; o código do app vem depois da linha
  ~9500.
- `android/`: reescrito do zero com a mesma ponte (`window.JMNative`) que a
  página usa.

## Montar

```bash
# Ferramentas oficiais do Android (sem Gradle): build-tools 34 e plataforma 34
#   https://dl.google.com/android/repository/build-tools_r34-linux.zip  -> /opt/android/android-14
#   https://dl.google.com/android/repository/platform-34-ext7_r02.zip   -> /opt/android/android-34
./build.sh            # gera build/J-Menseger-<versão>.apk
```

A chave de assinatura fica em `keystore/` (fora do git). **Guarde-a**: sem ela
não dá para lançar atualizações que instalem por cima.
